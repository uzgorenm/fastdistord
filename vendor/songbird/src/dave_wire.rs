//! DAVE wire adapters and bounded structural receive observations.
//! Key-package send contract follows libdave's bare KeyPackage marshal and
//! discord.js VoiceWebSocket.sendBinaryMessage (opcode + unchanged Davey payload).
//! https://github.com/discord/libdave/blob/main/cpp/test/external_sender.cpp
//! https://github.com/discordjs/discord.js/blob/main/packages/voice/src/networking/VoiceWebSocket.ts
/// Prepend opcode 26 to Davey's unchanged serialized KeyPackage.
/// No additional MLSMessage envelope or client sequence header is added.
pub fn key_package_frame(raw_package: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(1 + raw_package.len());
    frame.push(26);
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

/// Observe only JSON header opcode/length, then decode without retaining payload diagnostics.
pub fn decode_dave_json(
    payload: &str,
    handshake: &crate::DaveHandshake,
) -> std::result::Result<crate::model::Event, serde_json::Error> {
    #[derive(serde::Deserialize)]
    struct Header {
        op: u16,
    }
    let opcode = serde_json::from_str::<Header>(payload)
        .map(|h| h.op)
        .unwrap_or(u16::MAX);
    handshake.record(crate::DaveStage::JsonReceived, opcode);
    handshake.record(
        crate::DaveStage::JsonLength,
        payload.len().min(u16::MAX as usize) as u16,
    );
    let result = decode_json_envelope(payload);
    handshake.record(
        if result.is_ok() {
            crate::DaveStage::JsonDecoded
        } else {
            crate::DaveStage::JsonDecodeFailed
        },
        opcode,
    );
    result
}

// The pinned voice-model Event visitor returns immediately at `d`, and does not
// consume unknown envelope values. Normalize the envelope without copying its
// payload or relaxing payload validation. Unknown optional fields are skipped by
// derive; the borrowed RawValue is passed directly to the pinned payload decoder.
fn decode_json_envelope(
    payload: &str,
) -> std::result::Result<crate::model::Event, serde_json::Error> {
    use serde::{
        de::{
            value::{BorrowedStrDeserializer, MapDeserializer},
            IntoDeserializer,
        },
        Deserialize, Deserializer,
    };
    #[derive(Deserialize)]
    struct Envelope<'a> {
        op: u8,
        #[serde(borrow)]
        d: &'a serde_json::value::RawValue,
    }
    enum Field<'a> {
        Opcode(u8),
        Data(&'a serde_json::value::RawValue),
    }
    impl<'de> IntoDeserializer<'de, serde_json::Error> for Field<'de> {
        type Deserializer = Self;
        fn into_deserializer(self) -> Self {
            self
        }
    }
    impl<'de> Deserializer<'de> for Field<'de> {
        type Error = serde_json::Error;
        fn deserialize_any<V: serde::de::Visitor<'de>>(
            self,
            visitor: V,
        ) -> std::result::Result<V::Value, Self::Error> {
            match self {
                Self::Opcode(op) => visitor.visit_u8(op),
                Self::Data(raw) => raw.deserialize_any(visitor),
            }
        }
        serde::forward_to_deserialize_any! { bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string bytes byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map struct enum identifier ignored_any }
    }
    let Envelope { op, d } = serde_json::from_str(payload)?;
    let fields = [("op", Field::Opcode(op)), ("d", Field::Data(d))]
        .into_iter()
        .map(|(key, value)| {
            (
                BorrowedStrDeserializer::<serde_json::Error>::new(key),
                value,
            )
        });
    crate::model::Event::deserialize(MapDeserializer::new(fields))
}
