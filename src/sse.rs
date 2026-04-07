/// Extracts complete SSE events from a buffer, draining consumed data.
/// Returns the data payloads (with `data:` prefix stripped).
pub fn extract_events(buffer: &mut String) -> Vec<String> {
    let mut events = Vec::new();
    while let Some(idx) = buffer.find("\n\n") {
        let raw_event = buffer[..idx].to_string();
        buffer.drain(..idx + 2);

        let mut data_lines = Vec::new();
        for line in raw_event.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.trim_start().to_string());
            }
        }
        if !data_lines.is_empty() {
            events.push(data_lines.join("\n"));
        }
    }
    events
}

/// Appends raw bytes to the SSE string buffer, handling incomplete UTF-8
/// sequences at chunk boundaries. `residual` holds leftover bytes from
/// previous chunks that did not form valid UTF-8.
pub fn append_chunk(buffer: &mut String, residual: &mut Vec<u8>, chunk: &[u8]) {
    let mut bytes = std::mem::take(residual);
    bytes.extend_from_slice(chunk);

    match std::str::from_utf8(&bytes) {
        Ok(s) => {
            buffer.push_str(&s.replace("\r\n", "\n"));
        }
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            // valid_up_to guarantees this prefix is valid UTF-8
            let valid = std::str::from_utf8(&bytes[..valid_up_to])
                .expect("valid_up_to guarantees valid UTF-8 prefix");
            buffer.push_str(&valid.replace("\r\n", "\n"));
            *residual = bytes[valid_up_to..].to_vec();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_complete_event() {
        let mut buf = "data: {\"text\":\"hello\"}\n\n".to_string();
        let events = extract_events(&mut buf);
        assert_eq!(events, vec!["{\"text\":\"hello\"}"]);
        assert!(buf.is_empty());
    }

    #[test]
    fn partial_event_waits_for_double_newline() {
        let mut buf = "data: partial".to_string();
        let events = extract_events(&mut buf);
        assert!(events.is_empty());
        assert_eq!(buf, "data: partial");
    }

    #[test]
    fn multiple_events_in_one_chunk() {
        let mut buf = "data: first\n\ndata: second\n\n".to_string();
        let events = extract_events(&mut buf);
        assert_eq!(events, vec!["first", "second"]);
        assert!(buf.is_empty());
    }

    #[test]
    fn done_event_extracted_normally() {
        let mut buf = "data: [DONE]\n\n".to_string();
        let events = extract_events(&mut buf);
        assert_eq!(events, vec!["[DONE]"]);
    }

    #[test]
    fn crlf_normalization() {
        let mut buf = String::new();
        let mut residual = Vec::new();
        append_chunk(&mut buf, &mut residual, b"data: hello\r\n\r\n");
        let events = extract_events(&mut buf);
        assert_eq!(events, vec!["hello"]);
    }

    #[test]
    fn whitespace_only_buffer_yields_no_events() {
        let mut buf = "   \n  \n".to_string();
        let events = extract_events(&mut buf);
        assert!(events.is_empty());
    }

    #[test]
    fn multi_line_data_fields_joined() {
        let mut buf = "data: line1\ndata: line2\n\n".to_string();
        let events = extract_events(&mut buf);
        assert_eq!(events, vec!["line1\nline2"]);
    }

    #[test]
    fn incomplete_utf8_buffered_across_chunks() {
        let mut buf = String::new();
        let mut residual = Vec::new();
        // "é" is 0xC3 0xA9 in UTF-8. Split it across two chunks.
        append_chunk(
            &mut buf,
            &mut residual,
            &[b'd', b'a', b't', b'a', b':', b' ', 0xC3],
        );
        assert_eq!(residual.len(), 1);
        assert_eq!(buf, "data: ");
        append_chunk(&mut buf, &mut residual, &[0xA9, b'\n', b'\n']);
        assert!(residual.is_empty());
        let events = extract_events(&mut buf);
        assert_eq!(events, vec!["é"]);
    }

    #[test]
    fn empty_chunk_is_noop() {
        let mut buf = String::new();
        let mut residual = Vec::new();
        append_chunk(&mut buf, &mut residual, b"");
        assert!(buf.is_empty());
        assert!(residual.is_empty());
    }
}
