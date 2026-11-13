//! The lane and exact appliance identities carried into agent commands.

/// The variable naming the lane a command is bound to.
pub const LANE: &str = wire::SpiceVariable::Lane.as_str();

/// The exact Spice executable selected for the agent shell.
pub const EXECUTABLE: &str = wire::SpiceVariable::WrapperSpice.as_str();
