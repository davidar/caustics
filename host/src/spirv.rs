//! A specialisation constant set in the module's own words.
//!
//! A caustic's entry point can be built once per value of a specialisation
//! constant (`[[vk::constant_id(n)]] const uint … = …;` in Slang), so that
//! each pipeline carries one case's code and the driver folds the rest away.
//! Vulkan sets such a constant through `VkSpecializationInfo` when the
//! pipeline is created; wgpu does not pass one for a passthrough module (its
//! `PipelineCompilationOptions::constants` feed naga's override pass, which a
//! raw SPIR-V module never goes through). The constant's default value is
//! what a pipeline created without one uses, and it is a literal in the
//! module: so the value is written there, in a copy of the words, before the
//! module is created. The driver then sees a specialisation constant whose
//! value is known when the pipeline is built, which is the case it folds.
//!
//! Only a 32-bit integer constant is rewritten (`OpSpecConstant` of one
//! literal word); a boolean or a wider one is refused, as is a module whose
//! decoration names something that is not an `OpSpecConstant`.

use anyhow::{Result, bail};

/// The SPIR-V magic number.
const MAGIC: u32 = 0x0723_0203;
/// The header's length in words.
const HEADER: usize = 5;
const OP_DECORATE: u32 = 71;
const OP_SPEC_CONSTANT: u32 = 50;
/// `OpSpecConstantTrue` through `OpSpecConstantOp`: every instruction that can
/// define a specialisation constant, each with its result id second.
const OP_SPEC_CONSTANTS: std::ops::RangeInclusive<u32> = 48..=52;
const DECORATION_SPEC_ID: u32 = 1;

/// `spirv` with the specialisation constant whose `SpecId` is `id` set to
/// `value`, or `None` when the module declares no such constant (an entry
/// point that never reads it, whose every variant is the same pipeline).
pub fn specialise(spirv: &[u8], id: u32, value: u32) -> Result<Option<Vec<u8>>> {
    if spirv.len() % 4 != 0 || spirv.len() < HEADER * 4 {
        bail!("{} bytes is not a SPIR-V module", spirv.len());
    }
    let mut words: Vec<u32> = spirv
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect();
    if words[0] != MAGIC {
        bail!("the module does not start with the SPIR-V magic number");
    }

    // The decoration first: it names the result id the constant defines.
    let mut target = None;
    walk(&words, |at, opcode, operands| {
        if opcode == OP_DECORATE
            && operands.len() == 3
            && operands[1] == DECORATION_SPEC_ID
            && operands[2] == id
        {
            target = Some((at, operands[0]));
        }
    })?;
    let Some((_, result)) = target else {
        return Ok(None);
    };

    // Then the constant: `OpSpecConstant %type %result <literal>`.
    let mut literal = None;
    let mut wrong = None;
    walk(&words, |at, opcode, operands| {
        if OP_SPEC_CONSTANTS.contains(&opcode) && operands.get(1) == Some(&result) {
            match (opcode, operands.len()) {
                (OP_SPEC_CONSTANT, 3) => literal = Some(at + 3),
                (OP_SPEC_CONSTANT, n) => {
                    wrong = Some(format!(
                        "an OpSpecConstant of {} literal words, not one",
                        n.saturating_sub(2)
                    ))
                }
                (other, _) => wrong = Some(format!("opcode {other}, not OpSpecConstant")),
            }
        }
    })?;
    if let Some(wrong) = wrong {
        bail!("SpecId {id} decorates {wrong}; only a 32-bit integer constant is set here");
    }
    let Some(literal) = literal else {
        bail!("SpecId {id} decorates %{result}, which no specialisation constant defines");
    };
    words[literal] = value;
    Ok(Some(
        words.iter().flat_map(|word| word.to_le_bytes()).collect(),
    ))
}

/// `spirv-val`'s verdict on a module, or `None` where SPIRV-Tools is not
/// installed: what the device tests check a specialised module with, as
/// `punchcutter-slang` checks every module it compiles. Never called by a
/// host at run time.
pub fn validate(spirv: &[u8]) -> Option<Result<(), String>> {
    let path = std::env::temp_dir().join(format!(
        "caustics-specialised-{}-{:016x}.spv",
        std::process::id(),
        spirv.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        })
    ));
    std::fs::write(&path, spirv).ok()?;
    let output = std::process::Command::new("spirv-val").arg(&path).output();
    let _ = std::fs::remove_file(&path);
    let output = output.ok()?;
    if output.status.success() {
        Some(Ok(()))
    } else {
        Some(Err(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

/// Every instruction after the header: its first word's index, its opcode and
/// its operands.
fn walk(words: &[u32], mut each: impl FnMut(usize, u32, &[u32])) -> Result<()> {
    let mut at = HEADER;
    while at < words.len() {
        let count = (words[at] >> 16) as usize;
        let opcode = words[at] & 0xffff;
        if count == 0 || at + count > words.len() {
            bail!("a malformed instruction at word {at} (word count {count})");
        }
        each(at, opcode, &words[at + 1..at + count]);
        at += count;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
