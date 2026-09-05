use bytes::{Buf, BufMut, Bytes, BytesMut};
use pgwire::messages::data::{
    DataRow as PgWireDataRow, FieldDescription, RowDescription as PgWireRowDescription,
};
use pgwire::messages::extendedquery::{Flush as PgWireFlush, Sync as PgWireSync};
use pgwire::messages::response::{
    CommandComplete as PgWireCommandComplete, ErrorResponse as PgWireErrorResponse,
    NoticeResponse as PgWireNoticeResponse, ReadyForQuery as PgWireReadyForQuery,
};
use pgwire::messages::simplequery::Query as PgWireQuery;
use pgwire::messages::startup::{
    Authentication as PgWireAuthentication, BackendKeyData as PgWireBackendKeyData,
    ParameterStatus as PgWireParameterStatus, SecretKey,
};
use pgwire::messages::terminate::Terminate as PgWireTerminate;
use pgwire::messages::{DecodeContext, Message, PgWireBackendMessage, PgWireFrontendMessage};
use std::collections::HashMap;

pub use pgwire::messages::response::TransactionStatus;
pub const PROTOCOL_VERSION_3_0: i32 = pgwire::messages::startup::Startup::PROTOCOL_VERSION_3_0;
pub const SSL_REQUEST_CODE: i32 = 80877103;
pub const CANCEL_REQUEST_CODE: i32 = 80877102;

/// Helper extension trait for `TransactionStatus` to provide `from_u8` and `to_u8`.
pub trait TransactionStatusExt {
    fn from_u8(b: u8) -> Option<TransactionStatus>;
    fn to_u8(self) -> u8;
}

impl TransactionStatusExt for TransactionStatus {
    fn from_u8(b: u8) -> Option<TransactionStatus> {
        TransactionStatus::try_from(b).ok()
    }

    fn to_u8(self) -> u8 {
        self as u8
    }
}

/// Helper function to parse a transaction status byte.
pub fn parse_transaction_status(b: u8) -> Option<TransactionStatus> {
    TransactionStatus::try_from(b).ok()
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
    pub fn from_pgwire(startup: &pgwire::messages::startup::Startup) -> Self {
        let version =
            ((startup.protocol_number_major as i32) << 16) | (startup.protocol_number_minor as i32);
        let parameters = startup
            .parameters
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Self {
            protocol_version: version,
            parameters,
        }
    }

    pub fn to_pgwire(&self) -> pgwire::messages::startup::Startup {
        let mut s = pgwire::messages::startup::Startup::new();
        s.protocol_number_major = (self.protocol_version >> 16) as u16;
        s.protocol_number_minor = (self.protocol_version & 0xFFFF) as u16;
        s.parameters = self
            .parameters
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        s
    }

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

impl InitialClientMessage {
    /// Decodes the initial packet sent on a new client TCP connection using pgwire.
    pub fn decode(src: &mut BytesMut) -> Result<Option<Self>, std::io::Error> {
        if src.len() < 4 {
            return Ok(None);
        }
        let len = i32::from_be_bytes([src[0], src[1], src[2], src[3]]) as usize;
        if src.len() < len {
            return Ok(None);
        }

        if src.len() >= 8 {
            let code = i32::from_be_bytes([src[4], src[5], src[6], src[7]]);
            if len == 8 && code == SSL_REQUEST_CODE {
                src.advance(8);
                return Ok(Some(Self::SslRequest));
            }

            if len == 16 && code == CANCEL_REQUEST_CODE {
                let pid = u32::from_be_bytes([src[8], src[9], src[10], src[11]]);
                let secret = u32::from_be_bytes([src[12], src[13], src[14], src[15]]);
                src.advance(16);
                return Ok(Some(Self::CancelRequest {
                    process_id: pid,
                    secret_key: secret,
                }));
            }
        }

        let mut ctx = DecodeContext::default();
        ctx.awaiting_frontend_ssl = false;
        ctx.awaiting_frontend_startup = true;

        match PgWireFrontendMessage::decode(src, &ctx) {
            Ok(Some(PgWireFrontendMessage::Startup(startup))) => {
                Ok(Some(Self::Startup(StartupMessage::from_pgwire(&startup))))
            }
            Ok(Some(PgWireFrontendMessage::CancelRequest(cancel))) => {
                let secret = cancel.secret_key.as_i32().unwrap_or(0) as u32;
                Ok(Some(Self::CancelRequest {
                    process_id: cancel.pid as u32,
                    secret_key: secret,
                }))
            }
            Ok(_) => Ok(None),
            Err(e) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e.to_string(),
            )),
        }
    }

    /// Encodes a StartupMessage into bytes via pgwire.
    pub fn encode_startup(msg: &StartupMessage, dst: &mut BytesMut) {
        let pgwire_startup = msg.to_pgwire();
        let _ = pgwire_startup.encode(dst);
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
        target_type: u8,
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

impl FrontendMessage {
    /// Decodes a regular framed frontend message using pgwire.
    pub fn decode(src: &mut BytesMut) -> Result<Option<Self>, std::io::Error> {
        if src.len() < 5 {
            return Ok(None);
        }
        let tag = src[0];
        let len = i32::from_be_bytes([src[1], src[2], src[3], src[4]]) as usize;
        if src.len() < 1 + len {
            return Ok(None);
        }

        let mut ctx = DecodeContext::default();
        ctx.awaiting_frontend_ssl = false;
        ctx.awaiting_frontend_startup = false;

        match tag {
            b'Q' => match PgWireFrontendMessage::decode(src, &ctx) {
                Ok(Some(PgWireFrontendMessage::Query(q))) => Ok(Some(Self::Query(q.query))),
                Ok(_) => Ok(None),
                Err(e) => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    e.to_string(),
                )),
            },
            b'X' => match PgWireFrontendMessage::decode(src, &ctx) {
                Ok(Some(PgWireFrontendMessage::Terminate(_))) => Ok(Some(Self::Terminate)),
                Ok(_) => Ok(None),
                Err(e) => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    e.to_string(),
                )),
            },
            b'S' => match PgWireFrontendMessage::decode(src, &ctx) {
                Ok(Some(PgWireFrontendMessage::Sync(_))) => Ok(Some(Self::Sync)),
                Ok(_) => Ok(None),
                Err(e) => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    e.to_string(),
                )),
            },
            b'H' => match PgWireFrontendMessage::decode(src, &ctx) {
                Ok(Some(PgWireFrontendMessage::Flush(_))) => Ok(Some(Self::Flush)),
                Ok(_) => Ok(None),
                Err(e) => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    e.to_string(),
                )),
            },
            _ => {
                // Extended query packets or unknown tags: split frame and preserve raw payload
                let mut frame = src.split_to(1 + len);
                frame.advance(5); // Advance tag + length
                Ok(Some(Self::Raw {
                    tag,
                    payload: frame.freeze(),
                }))
            }
        }
    }

    /// Encodes a frontend message into framed bytes using pgwire.
    pub fn encode(&self, dst: &mut BytesMut) {
        match self {
            Self::Query(sql) => {
                let q = PgWireQuery::new(sql.clone());
                let _ = q.encode(dst);
            }
            Self::Sync => {
                let sync = PgWireSync::new();
                let _ = sync.encode(dst);
            }
            Self::Flush => {
                let flush = PgWireFlush::new();
                let _ = flush.encode(dst);
            }
            Self::Terminate => {
                let term = PgWireTerminate::new();
                let _ = term.encode(dst);
            }
            Self::Raw { tag, payload } => {
                dst.put_u8(*tag);
                dst.put_i32((payload.len() + 4) as i32);
                dst.put_slice(payload);
            }
            _ => {}
        }
    }
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
    RowDescription { columns: Vec<String> },
    DataRow { values: Vec<Option<String>> },
    Raw { tag: u8, payload: Bytes },
}

impl BackendMessage {
    /// Decodes a regular framed backend message using pgwire.
    pub fn decode(src: &mut BytesMut) -> Result<Option<Self>, std::io::Error> {
        if src.len() < 5 {
            return Ok(None);
        }
        let tag = src[0];
        let len = i32::from_be_bytes([src[1], src[2], src[3], src[4]]) as usize;
        if src.len() < 1 + len {
            return Ok(None);
        }

        let ctx = DecodeContext::default();
        match PgWireBackendMessage::decode(src, &ctx) {
            Ok(Some(msg)) => match msg {
                PgWireBackendMessage::Authentication(auth) => match auth {
                    PgWireAuthentication::Ok => Ok(Some(Self::AuthenticationOk)),
                    PgWireAuthentication::CleartextPassword => {
                        Ok(Some(Self::AuthenticationCleartextPassword))
                    }
                    PgWireAuthentication::MD5Password(salt) => {
                        let mut s = [0u8; 4];
                        if salt.len() >= 4 {
                            s.copy_from_slice(&salt[..4]);
                        }
                        Ok(Some(Self::AuthenticationMD5Password { salt: s }))
                    }
                    PgWireAuthentication::SASL(mechs) => {
                        Ok(Some(Self::AuthenticationSASL { mechanisms: mechs }))
                    }
                    PgWireAuthentication::SASLContinue(data) => {
                        Ok(Some(Self::AuthenticationSASLContinue {
                            data: data.to_vec(),
                        }))
                    }
                    PgWireAuthentication::SASLFinal(data) => {
                        Ok(Some(Self::AuthenticationSASLFinal {
                            data: data.to_vec(),
                        }))
                    }
                    _ => Ok(Some(Self::Raw {
                        tag,
                        payload: Bytes::new(),
                    })),
                },
                PgWireBackendMessage::ParameterStatus(ps) => Ok(Some(Self::ParameterStatus {
                    name: ps.name,
                    value: ps.value,
                })),
                PgWireBackendMessage::BackendKeyData(bk) => {
                    let secret = bk.secret_key.as_i32().unwrap_or(0) as u32;
                    Ok(Some(Self::BackendKeyData {
                        process_id: bk.pid as u32,
                        secret_key: secret,
                    }))
                }
                PgWireBackendMessage::ReadyForQuery(rfq) => {
                    Ok(Some(Self::ReadyForQuery { status: rfq.status }))
                }
                PgWireBackendMessage::CommandComplete(cc) => {
                    Ok(Some(Self::CommandComplete { tag: cc.tag }))
                }
                PgWireBackendMessage::ErrorResponse(er) => {
                    let message = er
                        .fields
                        .iter()
                        .find(|(k, _)| *k == b'M')
                        .map(|(_, v)| v.clone())
                        .unwrap_or_else(|| "Unknown database error".to_string());
                    Ok(Some(Self::ErrorResponse { message }))
                }
                PgWireBackendMessage::NoticeResponse(nr) => {
                    let message = nr
                        .fields
                        .iter()
                        .find(|(k, _)| *k == b'M')
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default();
                    Ok(Some(Self::NoticeResponse { message }))
                }
                PgWireBackendMessage::RowDescription(rd) => {
                    let columns = rd.fields.into_iter().map(|f| f.name).collect();
                    Ok(Some(Self::RowDescription { columns }))
                }
                PgWireBackendMessage::DataRow(dr) => {
                    let values = decode_data_row_values(&dr.data, dr.field_count);
                    Ok(Some(Self::DataRow { values }))
                }
                _ => Ok(Some(Self::Raw {
                    tag,
                    payload: Bytes::new(),
                })),
            },
            Ok(None) => Ok(None),
            Err(_) => {
                // If unrecognized by pgwire, fallback to Raw frame split
                let mut frame = src.split_to(1 + len);
                frame.advance(5);
                Ok(Some(Self::Raw {
                    tag,
                    payload: frame.freeze(),
                }))
            }
        }
    }

    /// Encodes a BackendMessage into bytes using pgwire.
    pub fn encode(&self, dst: &mut BytesMut) {
        match self {
            Self::AuthenticationOk => {
                let msg = PgWireAuthentication::Ok;
                let _ = msg.encode(dst);
            }
            Self::AuthenticationCleartextPassword => {
                let msg = PgWireAuthentication::CleartextPassword;
                let _ = msg.encode(dst);
            }
            Self::AuthenticationMD5Password { salt } => {
                let msg = PgWireAuthentication::MD5Password(salt.to_vec());
                let _ = msg.encode(dst);
            }
            Self::AuthenticationSASL { mechanisms } => {
                let msg = PgWireAuthentication::SASL(mechanisms.clone());
                let _ = msg.encode(dst);
            }
            Self::AuthenticationSASLContinue { data } => {
                let msg = PgWireAuthentication::SASLContinue(Bytes::copy_from_slice(data));
                let _ = msg.encode(dst);
            }
            Self::AuthenticationSASLFinal { data } => {
                let msg = PgWireAuthentication::SASLFinal(Bytes::copy_from_slice(data));
                let _ = msg.encode(dst);
            }
            Self::BackendKeyData {
                process_id,
                secret_key,
            } => {
                let msg = PgWireBackendKeyData::new(
                    *process_id as i32,
                    SecretKey::I32(*secret_key as i32),
                );
                let _ = msg.encode(dst);
            }
            Self::ParameterStatus { name, value } => {
                let msg = PgWireParameterStatus::new(name.clone(), value.clone());
                let _ = msg.encode(dst);
            }
            Self::ReadyForQuery { status } => {
                let msg = PgWireReadyForQuery::new(*status);
                let _ = msg.encode(dst);
            }
            Self::CommandComplete { tag } => {
                let msg = PgWireCommandComplete::new(tag.clone());
                let _ = msg.encode(dst);
            }
            Self::ErrorResponse { message } => {
                let msg = PgWireErrorResponse::new(vec![
                    (b'S', "ERROR".to_string()),
                    (b'C', "XX000".to_string()),
                    (b'M', message.clone()),
                ]);
                let _ = msg.encode(dst);
            }
            Self::NoticeResponse { message } => {
                let msg = PgWireNoticeResponse::new(vec![
                    (b'S', "NOTICE".to_string()),
                    (b'M', message.clone()),
                ]);
                let _ = msg.encode(dst);
            }
            Self::RowDescription { columns } => {
                let fields = columns
                    .iter()
                    .map(|col| {
                        let mut fd = FieldDescription::default();
                        fd.name = col.clone();
                        fd
                    })
                    .collect();
                let rd = PgWireRowDescription::new(fields);
                let _ = rd.encode(dst);
            }
            Self::DataRow { values } => {
                let mut data = BytesMut::new();
                for val in values {
                    match val {
                        None => data.put_i32(-1),
                        Some(s) => {
                            let b = s.as_bytes();
                            data.put_i32(b.len() as i32);
                            data.put_slice(b);
                        }
                    }
                }
                let dr = PgWireDataRow::new(data, values.len() as i16);
                let _ = dr.encode(dst);
            }
            Self::Raw { tag, payload } => {
                dst.put_u8(*tag);
                dst.put_i32((payload.len() + 4) as i32);
                dst.put_slice(payload);
            }
        }
    }
}

/// Helper to decode DataRow column values from pgwire's DataRow payload.
pub fn decode_data_row_values(data: &[u8], field_count: i16) -> Vec<Option<String>> {
    let mut buf = data;
    let mut values = Vec::with_capacity(field_count as usize);
    for _ in 0..field_count {
        if buf.len() < 4 {
            break;
        }
        let len = i32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        buf = &buf[4..];
        if len == -1 {
            values.push(None);
        } else if len >= 0 {
            let len = len as usize;
            if buf.len() < len {
                break;
            }
            let val = String::from_utf8_lossy(&buf[..len]).into_owned();
            buf = &buf[len..];
            values.push(Some(val));
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssl_request_decode() {
        let mut buf = BytesMut::new();
        buf.put_i32(8);
        buf.put_i32(SSL_REQUEST_CODE);

        let decoded = InitialClientMessage::decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, InitialClientMessage::SslRequest);
    }

    #[test]
    fn test_startup_message_encode_decode() {
        let mut params = HashMap::new();
        params.insert("user".to_string(), "postgres".to_string());
        params.insert("database".to_string(), "testdb".to_string());

        let startup = StartupMessage {
            protocol_version: PROTOCOL_VERSION_3_0,
            parameters: params,
        };

        let mut buf = BytesMut::new();
        InitialClientMessage::encode_startup(&startup, &mut buf);

        let decoded = InitialClientMessage::decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, InitialClientMessage::Startup(startup));
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

    #[test]
    fn test_row_description_and_data_row_encode_decode() {
        let desc = BackendMessage::RowDescription {
            columns: vec!["id".into(), "name".into()],
        };
        let mut buf = BytesMut::new();
        desc.encode(&mut buf);

        let decoded_desc = BackendMessage::decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded_desc, desc);

        let row = BackendMessage::DataRow {
            values: vec![Some("1".into()), Some("alpha".into())],
        };
        row.encode(&mut buf);

        let decoded_row = BackendMessage::decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded_row, row);
    }
}
