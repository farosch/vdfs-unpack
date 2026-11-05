//! Lowercase hex encoding for digests and link targets.

pub fn encode(data: &[u8]) -> String {
    data.iter().map(|value| format!("{value:02x}")).collect()
}

/// Test fixtures store byte strings as hex text.
#[cfg(test)]
pub fn decode(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}
