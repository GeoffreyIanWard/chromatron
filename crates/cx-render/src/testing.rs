//! Test support for code that needs a graphics device.
//!
//! Public rather than `#[cfg(test)]` because integration tests live in a
//! separate crate and cannot see this crate's private test items. It is a few
//! lines of glue, and having one copy is what keeps the skip policy consistent
//! across every test that needs a GPU.
//!
//! # The problem this exists to solve
//!
//! Renderer tests skip when no adapter is available, so that a bare container or
//! a machine without drivers stays usable. But cargo swallows a passing test's
//! output, which made "green" mean *either* "rendered and verified pixels" *or*
//! "found nothing and returned immediately" — with no way to tell which.
//!
//! That is the same failure shape as a benchmark gate that never runs: a check
//! reporting success without having checked. So CI sets `CX_REQUIRE_GPU=1`,
//! which turns a missing adapter into a failure. Locally the variable is unset
//! and the skip still applies.

use crate::device::RenderDevice;
use crate::error::RenderError;

/// The variable CI sets to forbid skipping.
pub const REQUIRE_GPU_VAR: &str = "CX_REQUIRE_GPU";

/// Whether a missing adapter should fail rather than skip.
///
/// Any value except `0` counts as set, so `CX_REQUIRE_GPU=1` and
/// `CX_REQUIRE_GPU=true` both work while `CX_REQUIRE_GPU=0` opts back out.
pub fn gpu_required() -> bool {
    std::env::var(REQUIRE_GPU_VAR).is_ok_and(|value| value != "0")
}

/// What to do when no adapter could be acquired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoAdapter {
    /// Skip the test; this environment is not expected to have a GPU.
    Skip(String),
    /// Fail the test; this environment was supposed to have one.
    Fail(String),
}

/// Decides between skipping and failing.
///
/// Separated from the environment lookup so both branches are testable on a
/// machine that *does* have a GPU. Otherwise the failing branch could only be
/// exercised by uninstalling a graphics driver, which means in practice it would
/// never be exercised at all.
pub fn decide_without_adapter(required: bool, error: &RenderError) -> NoAdapter {
    if required {
        NoAdapter::Fail(format!(
            "{REQUIRE_GPU_VAR} is set but no graphics adapter could be acquired: {error}\n\n\
             This environment is supposed to have one. On a Linux runner that means the Vulkan \
             software rasterizer is missing or broken — `mesa-vulkan-drivers` provides lavapipe. \
             Failing here rather than skipping is deliberate: a renderer test that quietly tests \
             nothing is worse than no test, because it reports green either way."
        ))
    } else {
        NoAdapter::Skip(format!("skipping: no graphics adapter ({error})"))
    }
}

/// A device for a test, or `None` when the test should skip.
///
/// Panics instead of returning `None` when [`gpu_required`] is set, so a CI run
/// that silently lost its graphics driver fails loudly rather than passing
/// having tested nothing.
#[must_use]
pub fn device_or_skip() -> Option<RenderDevice> {
    match RenderDevice::headless() {
        Ok(device) => Some(device),
        Err(error) => match decide_without_adapter(gpu_required(), &error) {
            NoAdapter::Fail(message) => panic!("{message}"),
            NoAdapter::Skip(message) => {
                println!("{message}");
                None
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_adapter_error() -> RenderError {
        RenderError::NoAdapter {
            reason: "no suitable adapter found".to_owned(),
        }
    }

    #[test]
    fn a_required_gpu_turns_a_missing_adapter_into_a_failure() {
        // The branch that cannot be reached on a machine with working drivers,
        // which is exactly why it is tested through the pure decision rather
        // than through the environment.
        let decision = decide_without_adapter(true, &no_adapter_error());

        let NoAdapter::Fail(message) = decision else {
            panic!("a required GPU must produce a failure, got {decision:?}");
        };
        assert!(
            message.contains(REQUIRE_GPU_VAR),
            "the message should name the variable"
        );
        assert!(
            message.contains("lavapipe"),
            "and say how to fix it on a runner"
        );
    }

    #[test]
    fn an_optional_gpu_skips_and_says_why() {
        let decision = decide_without_adapter(false, &no_adapter_error());

        let NoAdapter::Skip(message) = decision else {
            panic!("an optional GPU must skip, got {decision:?}");
        };
        assert!(
            message.contains("skipping"),
            "the skip should be visible in output"
        );
    }

    #[test]
    fn a_device_is_available_or_the_environment_says_it_need_not_be() {
        // Whichever branch runs, it is the correct one for this machine — which
        // is the property the whole module exists to make true.
        match device_or_skip() {
            Some(device) => println!("acquired: {}", device.info().summary()),
            None => assert!(
                !gpu_required(),
                "skipping is only allowed when a GPU is optional"
            ),
        }
    }
}
