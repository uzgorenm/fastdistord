//! DAVE opcode 26 carries an RFC 9420 MLSMessage, not a bare KeyPackage.
//! Davey 0.1.4 returns the latter. Keep this adapter at the WebSocket boundary.
//! Reference: https://github.com/discord/dave-protocol/blob/main/protocol.md#dave_mls_key_package-26
/// Wrap a Davey MLS 1.0 raw key package in the required client opcode 26 frame.
/// No server sequence number belongs in a client-to-server binary frame.
pub fn key_package_frame(raw_package: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + raw_package.len());
    // opcode 26, MLS protocol version 1 (u16), key_package wire format 5 (u16).
    frame.extend_from_slice(&[26, 0, 1, 0, 5]);
    frame.extend_from_slice(raw_package);
    frame
}
