use anyhow::{anyhow, Result};

/// Possible states for a Firecracker VM
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmState {
    /// VM has not been started yet
    NotStarted,
    /// VM is in the process of starting
    Starting,
    /// VM is running
    Running,
    /// VM is in the process of stopping
    Stopping,
    /// VM has been stopped
    Stopped,
    /// VM encountered an error
    Error,
}

impl std::fmt::Display for VmState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmState::NotStarted => write!(f, "Not Started"),
            VmState::Starting => write!(f, "Starting"),
            VmState::Running => write!(f, "Running"),
            VmState::Stopping => write!(f, "Stopping"),
            VmState::Stopped => write!(f, "Stopped"),
            VmState::Error => write!(f, "Error"),
        }
    }
}

/// Manages the lifecycle state transitions of a VM
pub struct VmLifecycle {
    state: VmState,
}

impl VmLifecycle {
    /// Create a new lifecycle manager in the NotStarted state
    pub fn new() -> Self {
        Self {
            state: VmState::NotStarted,
        }
    }

    /// Get the current state
    pub fn current_state(&self) -> VmState {
        self.state
    }

    /// Transition to a new state, validating the transition is legal
    pub fn transition_to(&mut self, new_state: VmState) -> Result<()> {
        // Validate state transition
        let is_valid = match (self.state, new_state) {
            // Can always transition to error state
            (_, VmState::Error) => true,

            // Normal forward transitions
            (VmState::NotStarted, VmState::Starting) => true,
            (VmState::Starting, VmState::Running) => true,
            (VmState::Running, VmState::Stopping) => true,
            (VmState::Stopping, VmState::Stopped) => true,

            // Allow restarting from stopped
            (VmState::Stopped, VmState::Starting) => true,

            // Allow stopping from starting if something goes wrong
            (VmState::Starting, VmState::Stopping) => true,
            (VmState::Starting, VmState::Stopped) => true,

            // Same state transition is a no-op
            (old, new) if old == new => true,

            // All other transitions are invalid
            _ => false,
        };

        if !is_valid {
            return Err(anyhow!(
                "Invalid state transition from {} to {}",
                self.state,
                new_state
            ));
        }

        self.state = new_state;
        Ok(())
    }
}

impl Default for VmLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        let mut lifecycle = VmLifecycle::new();

        assert_eq!(lifecycle.current_state(), VmState::NotStarted);

        lifecycle.transition_to(VmState::Starting).unwrap();
        assert_eq!(lifecycle.current_state(), VmState::Starting);

        lifecycle.transition_to(VmState::Running).unwrap();
        assert_eq!(lifecycle.current_state(), VmState::Running);

        lifecycle.transition_to(VmState::Stopping).unwrap();
        assert_eq!(lifecycle.current_state(), VmState::Stopping);

        lifecycle.transition_to(VmState::Stopped).unwrap();
        assert_eq!(lifecycle.current_state(), VmState::Stopped);
    }

    #[test]
    fn test_invalid_transition() {
        let mut lifecycle = VmLifecycle::new();

        // Cannot go directly from NotStarted to Running
        assert!(lifecycle.transition_to(VmState::Running).is_err());
    }

    #[test]
    fn test_error_state_always_valid() {
        let mut lifecycle = VmLifecycle::new();

        // Can transition to error from any state
        lifecycle.transition_to(VmState::Error).unwrap();
        assert_eq!(lifecycle.current_state(), VmState::Error);
    }
}
