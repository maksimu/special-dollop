use bytes::Bytes;
use guacr_protocol::GuacamoleParser;

#[test]
fn test_optimized_img_instruction() {
    // This is the format our optimized code generates
    let instruction = "3.img,1.1,2.15,1.0,9.image/png,3.826,3.958;";
    let bytes = Bytes::from(instruction.to_string());

    let result = GuacamoleParser::parse_instruction(&bytes).unwrap();
    assert_eq!(result.opcode, "img");
    assert_eq!(result.args, vec!["1", "15", "0", "image/png", "826", "958"]);
}

#[test]
fn test_optimized_blob_instruction() {
    let instruction = "4.blob,1.1,4.test;";
    let bytes = Bytes::from(instruction.to_string());

    let result = GuacamoleParser::parse_instruction(&bytes).unwrap();
    assert_eq!(result.opcode, "blob");
    assert_eq!(result.args, vec!["1", "test"]);
}

#[test]
fn test_optimized_sync_instruction() {
    let instruction = "4.sync,13.1737936000000;";
    let bytes = Bytes::from(instruction.to_string());

    let result = GuacamoleParser::parse_instruction(&bytes).unwrap();
    assert_eq!(result.opcode, "sync");
    assert_eq!(result.args, vec!["1737936000000"]);
}

#[test]
fn test_optimized_ready_instruction() {
    let instruction = "5.ready,9.rdp-ready;";
    let bytes = Bytes::from(instruction.to_string());

    let result = GuacamoleParser::parse_instruction(&bytes).unwrap();
    assert_eq!(result.opcode, "ready");
    assert_eq!(result.args, vec!["rdp-ready"]);
}

#[test]
fn test_optimized_size_instruction() {
    let instruction = "4.size,1.0,4.1920,4.1080;";
    let bytes = Bytes::from(instruction.to_string());

    let result = GuacamoleParser::parse_instruction(&bytes).unwrap();
    assert_eq!(result.opcode, "size");
    assert_eq!(result.args, vec!["0", "1920", "1080"]);
}
