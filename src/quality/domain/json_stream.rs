use serde_json::{Deserializer, Value};

pub fn json_documents(text: &str) -> Vec<Value> {
    let mut documents = Vec::new();
    let mut offset = 0;
    while offset < text.len() {
        let rest = &text[offset..];
        if rest.starts_with(['{', '[']) {
            let mut stream = Deserializer::from_str(rest).into_iter::<Value>();
            if let Some(Ok(document)) = stream.next() {
                documents.push(document);
                offset += stream.byte_offset();
            }
        }
        offset = match text[offset..].find('\n') {
            Some(newline) => offset + newline + 1,
            None => break,
        };
    }
    documents
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_one_document_among_noise() {
        let text = "starting\n[INFO] x\n{\"a\": 1}\ndone\n";
        assert_eq!(json_documents(text), vec![serde_json::json!({"a": 1})]);
    }

    #[test]
    fn reads_a_stream_of_pretty_printed_documents() {
        let text = "{\n  \"config\": {}\n}\n{\n  \"finding\": {\"osv\": \"X\"}\n}\n[]\n";
        let docs = json_documents(text);
        assert_eq!(docs.len(), 3);
        assert_eq!(docs[1]["finding"]["osv"], "X");
        assert_eq!(docs[2], serde_json::json!([]));
    }

    #[test]
    fn skips_broken_documents_and_empty_input() {
        assert!(json_documents("").is_empty());
        assert!(json_documents("{ not json\nplain\n").is_empty());
        assert_eq!(json_documents("{bad\n{\"ok\":true}").len(), 1);
    }

    #[test]
    fn two_documents_on_one_line_keep_only_the_first_per_line() {
        assert_eq!(json_documents("{\"a\":1}{\"b\":2}\n").len(), 1);
    }
}
