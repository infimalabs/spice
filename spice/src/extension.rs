//! Fixed tables for the vendors and component names compiled into this release.
//!
//! These entries cannot change at run time. Consumers select directly from the
//! relevant table; there is no discovery or validation layer between compiled
//! values and the code that uses them.

use std::fmt;
use std::ops::Deref;

/// Typed configuration fold owned by one compiled vendor.
type ConfigFold = for<'a> fn(&'a crate::config::Root) -> vendor::LaunchValues<'a>;

/// Session behavior carried by one compiled vendor value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionGate {
    identity: &'static str,
    dialect: listen::Dialect,
}

impl SessionGate {
    const fn new(identity: &'static str, dialect: listen::Dialect) -> Self {
        Self { identity, dialect }
    }

    /// Stable identity retained in session reports.
    #[must_use]
    pub const fn identity(self) -> &'static str {
        self.identity
    }

    /// Transcript decoder selected for declared sources.
    #[must_use]
    pub const fn dialect(self) -> listen::Dialect {
        self.dialect
    }
}

/// One complete compiled vendor, including configuration and session behavior.
#[derive(Clone, Copy)]
pub struct Vendor {
    dialect: &'static vendor::Vendor,
    configuration: ConfigFold,
    session_gate: SessionGate,
}

impl Vendor {
    const fn new(dialect: &'static vendor::Vendor, configuration: ConfigFold) -> Self {
        Self {
            dialect,
            configuration,
            session_gate: SessionGate::new(dialect.name(), dialect.dialect()),
        }
    }

    /// Project launch values from the authoritative configuration root.
    #[must_use]
    pub fn launch_values<'a>(&self, root: &'a crate::config::Root) -> vendor::LaunchValues<'a> {
        (self.configuration)(root)
    }

    /// Vendor-owned session contract.
    #[must_use]
    pub const fn session_gate(&self) -> SessionGate {
        self.session_gate
    }
}

impl Deref for Vendor {
    type Target = vendor::Vendor;

    fn deref(&self) -> &Self::Target {
        self.dialect
    }
}

impl fmt::Debug for Vendor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.dialect.fmt(formatter)
    }
}

impl PartialEq for Vendor {
    fn eq(&self, other: &Self) -> bool {
        self.dialect == other.dialect
    }
}

impl Eq for Vendor {}

/// Current oh-my-pi vendor.
pub const OMP: Vendor = Vendor::new(&vendor::OMP, omp_configuration);

/// Scripted oh-my-pi vendor. Its occupant carries the omp session variable, so
/// only `OMP` appears in [`SESSION_VENDORS`].
pub const OMP_SCRIPTED: Vendor = Vendor::new(&vendor::OMP_SCRIPTED, omp_configuration);

/// One vendor and the compiled component that supplies it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VendorEntry {
    /// Complete vendor contract.
    pub vendor: &'static Vendor,
    /// Distribution or component supplying the contract.
    pub provider: &'static str,
}

/// Vendors this release can launch and ingest, in stable display order.
pub const VENDORS: &[VendorEntry] = &[
    VendorEntry {
        vendor: &OMP,
        provider: "vendor",
    },
    VendorEntry {
        vendor: &OMP_SCRIPTED,
        provider: "vendor",
    },
];

/// Vendors that own ambient session variables, in precedence order.
pub const SESSION_VENDORS: &[&Vendor] = &[&OMP];

/// One compiled component named in the Doctor report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Component {
    /// Stable component name.
    pub name: &'static str,
    /// Distribution or component supplying the contract.
    pub provider: &'static str,
}

/// Ambient Spice command wrapper compiled into this release.
pub const SHELL_WRAPPER: Component = Component {
    name: "shell",
    provider: "steer::hook",
};

/// Repository gates compiled into this release, in stable display order.
pub const GATES: &[Component] = &[
    Component {
        name: "core",
        provider: "gate::core",
    },
    Component {
        name: "hygiene",
        provider: "gate::hygiene",
    },
    Component {
        name: "toolchain",
        provider: "gate::toolchain",
    },
];

fn omp_configuration(root: &crate::config::Root) -> vendor::LaunchValues<'_> {
    let configured = root.vendor().omp();
    vendor::LaunchValues {
        binary: configured.binary().as_str(),
        model: configured.model(),
        effort: configured.effort(),
        fast_mode: Some(configured.fast_mode()),
        auto_compact_window_tokens: Some(configured.auto_compact_window_tokens()),
        lsp: Some(configured.lsp()),
    }
}
