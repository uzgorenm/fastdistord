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

/// Decode opcode-first Gateway v4 DAVE binary frames with bounded structural observations.
/// Candidate sequenced-header observations never change parsing or readiness.
pub fn decode_dave_binary(
    bytes: &[u8],
    handshake: &crate::DaveHandshake,
) -> std::result::Result<crate::model::Event, serenity_voice_model::BinaryError> {
    let opcode = bytes
        .first()
        .copied()
        .filter(|op| matches!(op, 25 | 27 | 29 | 30))
        .unwrap_or(0);
    handshake.record(crate::DaveStage::BinaryReceived, u16::from(opcode));
    handshake.record(
        crate::DaveStage::BinaryLength,
        bytes.len().min(u16::MAX as usize) as u16,
    );
    if let Some(opcode) = bytes
        .get(2)
        .copied()
        .filter(|op| opcode == 0 && matches!(op, 25 | 27 | 29 | 30))
    {
        handshake.record(crate::DaveStage::SequencedOpcode, u16::from(opcode));
    }
    let result = crate::model::deserialize_binary_event(bytes);
    match &result {
        Ok(_) => handshake.record(crate::DaveStage::BinaryDecoded, u16::from(opcode)),
        Err(error) => handshake.record(
            crate::DaveStage::BinaryDecodeFailed,
            match error {
                serenity_voice_model::BinaryError::InsufficientData => 1,
                serenity_voice_model::BinaryError::InvalidOpcode(_) => 2,
                serenity_voice_model::BinaryError::InvalidOperationType(_) => 3,
                serenity_voice_model::BinaryError::ParseError(_) => 4,
            },
        ),
    }
    result
}
