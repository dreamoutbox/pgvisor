use bytes::BytesMut;

/// Extracts a single PostgreSQL wire protocol frame from `buf` if complete.
/// A complete frame consists of 1 byte tag + 4 bytes length + (length - 4) payload bytes.
///
/// # Arguments
/// * `buf` - Mutable byte buffer containing incoming wire protocol bytes.
///
/// # Example
/// ```
/// use bytes::BytesMut;
/// use pgvisor_core::pg_client::extract_backend_frame;
///
/// let mut buf = BytesMut::from(&[b'Z', 0, 0, 0, 5, b'I'][..]);
/// let (tag, frame) = extract_backend_frame(&mut buf).unwrap();
/// assert_eq!(tag, b'Z');
/// assert_eq!(frame.len(), 6);
/// ```
pub fn extract_backend_frame(buf: &mut BytesMut) -> Option<(u8, BytesMut)> {
    if buf.len() < 5 {
        return None;
    }
    let tag = buf[0];
    let len = i32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    if buf.len() < 1 + len {
        return None;
    }
    Some((tag, buf.split_to(1 + len)))
}
