use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const SSL_REQUEST_CODE: i32 = 80877103;
pub const CANCEL_REQUEST_CODE: i32 = 80877102;
pub const PROTOCOL_VERSION_3_0: i32 = 196608;

/// PostgreSQL transaction status indicator reported in ReadyForQuery ('Z').
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionStatus {
    /// Not in a transaction block ('I').
    Idle,
    /// Inside an active transaction block ('T').
    Transaction,
    /// Inside a failed transaction block ('E').
    Error,
}

impl TransactionStatus {
    pub fn from_u8(b: u8) -> Option<Self> {
        match b {
            b'I' => Some(Self::Idle),
            b'T' => Some(Self::Transaction),
            b'E' => Some(Self::Error),
            _ => None,
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            Self::Idle => b'I',
            Self::Transaction => b'T',
            Self::Error => b'E',
        }
    }
}

/// Initial message sent by a PostgreSQL client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitialClientMessage {
    Startup(StartupMessage),
    SslRequest,
    CancelRequest { process_id: u32, secret_key: u32 },
}

/// PostgreSQL connection parameters sent in StartupMessage.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StartupMessage {
    pub protocol_version: i32,
    pub parameters: HashMap<String, String>,
}

impl StartupMessage {
    pub fn user(&self) -> Option<&str> {
        self.parameters.get("user").map(|s| s.as_str())
    }

    pub fn database(&self) -> Option<&str> {
        self.parameters.get("database").map(|s| s.as_str())
    }

    pub fn application_name(&self) -> Option<&str> {
        self.parameters.get("application_name").map(|s| s.as_str())
    }
}

/// Decoded client-to-server (frontend) messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontendMessage {
    Query(String),
    Parse {
        name: String,
        query: String,
        param_types: Vec<u32>,
    },
    Bind {
        portal: String,
        statement: String,
    },
    Describe {
        target_type: u8, // 'S' for prepared statement, 'P' for portal
        name: String,
    },
    Execute {
        portal: String,
        max_rows: i32,
    },
    Sync,
    Flush,
    Close {
        target_type: u8,
        name: String,
    },
    Password(String),
    Terminate,
    Raw {
        tag: u8,
        payload: Bytes,
    },
}

/// Decoded server-to-client (backend) messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendMessage {
    AuthenticationOk,
    AuthenticationCleartextPassword,
    AuthenticationMD5Password { salt: [u8; 4] },
    AuthenticationSASL { mechanisms: Vec<String> },
    AuthenticationSASLContinue { data: Vec<u8> },
    AuthenticationSASLFinal { data: Vec<u8> },
    BackendKeyData { process_id: u32, secret_key: u32 },
    ParameterStatus { name: String, value: String },
    ReadyForQuery { status: TransactionStatus },
    CommandComplete { tag: String },
    ErrorResponse { message: String },
    NoticeResponse { message: String },
    Raw { tag: u8, payload: Bytes },
}

impl InitialClientMessage {
    /// Decodes the initial packet sent on a new client TCP connection.
    pub fn decode(src: &mut BytesMut) -> Result<Option<Self>, std::io::Error> {
        if src.len() < 4 {
            return Ok(None);
        }

        let len = i32::from_be_bytes([src[0], src[1], src[2], src[3]]) as usize;
        if src.len() < len {
            return Ok(None);
        }

        let mut packet = src.split_to(len);
        packet.advance(4); // Advance past length

        let code = packet.get_i32();
        if code == SSL_REQUEST_CODE {
            return Ok(Some(Self::SslRequest));
        }

        if code == CANCEL_REQUEST_CODE {
            if packet.remaining() < 8 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Invalid CancelRequest length",
                ));
            }
            let process_id = packet.get_u32();
            let secret_key = packet.get_u32();
            return Ok(Some(Self::CancelRequest {
                process_id,
                secret_key,
            }));
        }

        // Standard StartupMessage: null-terminated strings key\0value\0... until \0
        let mut parameters = HashMap::new();
        while packet.has_remaining() {
            if packet[0] == 0 {
                packet.advance(1);
                break;
            }

            let key = read_null_terminated_string(&mut packet)?;
            if key.is_empty() {
                break;
            }
            let value = read_null_terminated_string(&mut packet)?;
            parameters.insert(key, value);
        }

        Ok(Some(Self::Startup(StartupMessage {
            protocol_version: code,
            parameters,
        })))
    }

    /// Encodes a StartupMessage into bytes.
    pub fn encode_startup(msg: &StartupMessage, dst: &mut BytesMut) {
        let mut body = BytesMut::new();
        body.put_i32(msg.protocol_version);
        for (k, v) in &msg.parameters {
            body.put_slice(k.as_bytes());
            body.put_u8(0);
            body.put_slice(v.as_bytes());
            body.put_u8(0);
        }
        body.put_u8(0); // Final terminating null

        let len = (body.len() + 4) as i32;
        dst.put_i32(len);
        dst.put_slice(&body);
    }
}

impl FrontendMessage {
    /// Decodes a regular framed frontend message (1 byte tag + 4 bytes length + payload).
    pub fn decode(src: &mut BytesMut) -> Result<Option<Self>, std::io::Error> {
        if src.len() < 5 {
            return Ok(None);
        }

        let tag = src[0];
        let len = i32::from_be_bytes([src[1], src[2], src[3], src[4]]) as usize;
        if src.len() < 1 + len {
            return Ok(None);
        }

        let mut frame = src.split_to(1 + len);
        frame.advance(5); // Advance tag + length
        let payload = frame.freeze();

        let msg = match tag {
            b'Q' => {
                let sql = std::str::from_utf8(&payload)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
                    .trim_end_matches('\0')
                    .to_string();
                Self::Query(sql)
            }
            b'S' => Self::Sync,
            b'H' => Self::Flush,
            b'X' => Self::Terminate,
            b'p' => {
                let pwd = std::str::from_utf8(&payload)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
                    .trim_end_matches('\0')
                    .to_string();
                Self::Password(pwd)
            }
            _ => Self::Raw { tag, payload },
        };

        Ok(Some(msg))
    }

    /// Encodes a frontend message into framed bytes.
    pub fn encode(&self, dst: &mut BytesMut) {
        match self {
            Self::Query(sql) => {
                dst.put_u8(b'Q');
                let len = (4 + sql.len() + 1) as i32;
                dst.put_i32(len);
                dst.put_slice(sql.as_bytes());
                dst.put_u8(0);
            }
            Self::Sync => {
                dst.put_u8(b'S');
                dst.put_i32(4);
            }
            Self::Flush => {
                dst.put_u8(b'H');
                dst.put_i32(4);
            }
            Self::Terminate => {
                dst.put_u8(b'X');
                dst.put_i32(4);
            }
            Self::Password(pwd) => {
                dst.put_u8(b'p');
                let len = (4 + pwd.len() + 1) as i32;
                dst.put_i32(len);
                dst.put_slice(pwd.as_bytes());
                dst.put_u8(0);
            }
            Self::Raw { tag, payload } => {
                dst.put_u8(*tag);
                let len = (4 + payload.len()) as i32;
                dst.put_i32(len);
                dst.put_slice(payload);
            }
            _ => {
                // Extended query variants can be serialized as Raw when transparently proxied
            }
        }
    }
}

impl BackendMessage {
    /// Decodes a regular framed backend message (1 byte tag + 4 bytes length + payload).
    pub fn decode(src: &mut BytesMut) -> Result<Option<Self>, std::io::Error> {
        if src.len() < 5 {
            return Ok(None);
        }

        let tag = src[0];
        let len = i32::from_be_bytes([src[1], src[2], src[3], src[4]]) as usize;
        if src.len() < 1 + len {
            return Ok(None);
        }

        let mut frame = src.split_to(1 + len);
        frame.advance(5);
        let mut payload = frame.freeze();

        let msg = match tag {
            b'R' => {
                let auth_type = payload.get_i32();
                match auth_type {
                    0 => Self::AuthenticationOk,
                    3 => Self::AuthenticationCleartextPassword,
                    5 => {
                        let mut salt = [0u8; 4];
                        if payload.remaining() >= 4 {
                            payload.copy_to_slice(&mut salt);
                        }
                        Self::AuthenticationMD5Password { salt }
                    }
                    _ => Self::Raw {
                        tag,
                        payload: payload.clone(),
                    },
                }
            }
            b'K' => {
                if payload.remaining() >= 8 {
                    let process_id = payload.get_u32();
                    let secret_key = payload.get_u32();
                    Self::BackendKeyData {
                        process_id,
                        secret_key,
                    }
                } else {
                    Self::Raw { tag, payload }
                }
            }
            b'S' => {
                let mut buf = payload.clone();
                let name = read_null_terminated_bytes(&mut buf).unwrap_or_default();
                let value = read_null_terminated_bytes(&mut buf).unwrap_or_default();
                Self::ParameterStatus { name, value }
            }
            b'Z' => {
                let status_byte = if payload.has_remaining() {
                    payload.get_u8()
                } else {
                    b'I'
                };
                let status =
                    TransactionStatus::from_u8(status_byte).unwrap_or(TransactionStatus::Idle);
                Self::ReadyForQuery { status }
            }
            b'C' => {
                let tag_str = std::str::from_utf8(&payload)
                    .unwrap_or_default()
                    .trim_end_matches('\0')
                    .to_string();
                Self::CommandComplete { tag: tag_str }
            }
            b'E' => {
                let err_str = std::str::from_utf8(&payload)
                    .unwrap_or_default()
                    .to_string();
                Self::ErrorResponse { message: err_str }
            }
            b'N' => {
                let msg_str = std::str::from_utf8(&payload)
                    .unwrap_or_default()
                    .to_string();
                Self::NoticeResponse { message: msg_str }
            }
            _ => Self::Raw { tag, payload },
        };

        Ok(Some(msg))
    }

    /// Encodes a backend message into framed bytes.
    pub fn encode(&self, dst: &mut BytesMut) {
        match self {
            Self::AuthenticationOk => {
                dst.put_u8(b'R');
                dst.put_i32(8);
                dst.put_i32(0);
            }
            Self::ReadyForQuery { status } => {
                dst.put_u8(b'Z');
                dst.put_i32(5);
                dst.put_u8(status.to_u8());
            }
            Self::CommandComplete { tag } => {
                dst.put_u8(b'C');
                let len = (4 + tag.len() + 1) as i32;
                dst.put_i32(len);
                dst.put_slice(tag.as_bytes());
                dst.put_u8(0);
            }
            Self::ParameterStatus { name, value } => {
                dst.put_u8(b'S');
                let len = (4 + name.len() + 1 + value.len() + 1) as i32;
                dst.put_i32(len);
                dst.put_slice(name.as_bytes());
                dst.put_u8(0);
                dst.put_slice(value.as_bytes());
                dst.put_u8(0);
            }
            Self::ErrorResponse { message } => {
                dst.put_u8(b'E');
                let body = format!("SERROR\0C57P01\0M{}\0\0", message);
                let len = (4 + body.len()) as i32;
                dst.put_i32(len);
                dst.put_slice(body.as_bytes());
            }
            Self::NoticeResponse { message } => {
                dst.put_u8(b'N');
                let body = format!("SNOTICE\0M{}\0\0", message);
                let len = (4 + body.len()) as i32;
                dst.put_i32(len);
                dst.put_slice(body.as_bytes());
            }
            Self::BackendKeyData {
                process_id,
                secret_key,
            } => {
                dst.put_u8(b'K');
                dst.put_i32(12);
                dst.put_u32(*process_id);
                dst.put_u32(*secret_key);
            }
            Self::Raw { tag, payload } => {
                dst.put_u8(*tag);
                let len = (4 + payload.len()) as i32;
                dst.put_i32(len);
                dst.put_slice(payload);
            }
            _ => {}
        }
    }
}

fn read_null_terminated_string(buf: &mut BytesMut) -> Result<String, std::io::Error> {
    if let Some(pos) = buf.iter().position(|&b| b == 0) {
        let s = std::str::from_utf8(&buf[..pos])
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
            .to_string();
        buf.advance(pos + 1);
        Ok(s)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "Missing null terminator in string",
        ))
    }
}

fn read_null_terminated_bytes(buf: &mut Bytes) -> Option<String> {
    if let Some(pos) = buf.iter().position(|&b| b == 0) {
        let s = std::str::from_utf8(&buf[..pos]).ok()?.to_string();
        buf.advance(pos + 1);
        Some(s)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_startup_message_encode_decode() {
        let mut startup = StartupMessage {
            protocol_version: PROTOCOL_VERSION_3_0,
            parameters: HashMap::new(),
        };
        startup.parameters.insert("user".into(), "postgres".into());
        startup
            .parameters
            .insert("database".into(), "testdb".into());

        let mut buf = BytesMut::new();
        InitialClientMessage::encode_startup(&startup, &mut buf);

        let decoded = InitialClientMessage::decode(&mut buf).unwrap().unwrap();
        if let InitialClientMessage::Startup(msg) = decoded {
            assert_eq!(msg.protocol_version, PROTOCOL_VERSION_3_0);
            assert_eq!(msg.user(), Some("postgres"));
            assert_eq!(msg.database(), Some("testdb"));
        } else {
            panic!("expected Startup message");
        }
    }

    #[test]
    fn test_ssl_request_decode() {
        let mut buf = BytesMut::new();
        buf.put_i32(8);
        buf.put_i32(SSL_REQUEST_CODE);

        let decoded = InitialClientMessage::decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, InitialClientMessage::SslRequest);
    }

    #[test]
    fn test_frontend_query_encode_decode() {
        let query = FrontendMessage::Query("SELECT 1;".into());
        let mut buf = BytesMut::new();
        query.encode(&mut buf);

        let decoded = FrontendMessage::decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, FrontendMessage::Query("SELECT 1;".into()));
    }

    #[test]
    fn test_backend_ready_for_query_encode_decode() {
        let ready = BackendMessage::ReadyForQuery {
            status: TransactionStatus::Transaction,
        };
        let mut buf = BytesMut::new();
        ready.encode(&mut buf);

        let decoded = BackendMessage::decode(&mut buf).unwrap().unwrap();
        assert_eq!(
            decoded,
            BackendMessage::ReadyForQuery {
                status: TransactionStatus::Transaction
            }
        );
    }

    #[test]
    fn test_backend_key_data_encode_decode() {
        let key_data = BackendMessage::BackendKeyData {
            process_id: 12345,
            secret_key: 67890,
        };
        let mut buf = BytesMut::new();
        key_data.encode(&mut buf);

        let decoded = BackendMessage::decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, key_data);
    }

    #[test]
    fn test_notice_response_decode() {
        let notice = BackendMessage::NoticeResponse {
            message: "table does not exist".into(),
        };
        let mut buf = BytesMut::new();
        notice.encode(&mut buf);

        let decoded = BackendMessage::decode(&mut buf).unwrap().unwrap();
        if let BackendMessage::NoticeResponse { message } = decoded {
            assert!(message.contains("table does not exist"));
        } else {
            panic!("expected NoticeResponse message");
        }
    }
}
