//! The `Module` trait and what a module registers.

use cx_ecs::{IntoScheduleConfigs, Phase, ScheduleSystem, SimSchedule};

use crate::capability::{Capability, Degradation};

/// A stable module identity.
///
/// Part of world identity (`ADR-0012`): the same seed with erosion on and off is
/// a different world, so this string is recorded in saves and replays. Renaming
/// a module invalidates existing saves, which is why it is written explicitly
/// rather than derived from a type name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId(pub &'static str);

impl ModuleId {
    /// The identifier text.
    pub const fn name(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for ModuleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// A module version, recorded in saves alongside the module set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Incompatible change.
    pub major: u16,
    /// Compatible addition.
    pub minor: u16,
}

impl Version {
    /// A version.
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// A subsystem that can be enabled, disabled, or developed in isolation.
///
/// Everything a module contributes is declared in one place, which is what makes
/// the S21 architecture graph a projection of reality rather than a drawing.
pub trait Module: 'static {
    /// Stable identity.
    const ID: ModuleId;

    /// Version, recorded as part of world identity.
    const VERSION: Version = Version::new(0, 1);

    /// What other modules may rely on this one for.
    fn provides() -> &'static [Capability] {
        &[]
    }

    /// Hard dependencies. A missing one is a startup error, not a degradation.
    fn requires() -> &'static [Capability] {
        &[]
    }

    /// Soft dependencies. Each must have a documented [`Module::degradations`]
    /// entry, and resolution fails if one does not.
    fn consumes_optional() -> &'static [Capability] {
        &[]
    }

    /// What this module does when each optional capability is absent.
    fn degradations() -> &'static [Degradation] {
        &[]
    }

    /// Registers fields, systems, and resources.
    fn register(registrar: &mut Registrar);
}

/// A system a module contributes to the tick.
pub struct SystemDecl {
    /// Unique within the module. Appears in the schedule hash and the S21 graph.
    pub name: &'static str,
    /// Which phase it runs in.
    pub phase: Phase,
    /// The module that registered it.
    pub module: ModuleId,
    install: Box<dyn FnOnce(&mut SimSchedule) + Send + Sync>,
}

impl std::fmt::Debug for SystemDecl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemDecl")
            .field("name", &self.name)
            .field("phase", &self.phase)
            .field("module", &self.module)
            .finish_non_exhaustive()
    }
}

impl SystemDecl {
    /// Installs the system into a schedule. Consumes the declaration.
    pub fn install(self, schedule: &mut SimSchedule) {
        (self.install)(schedule);
    }
}

/// How a system touches a field (S21).
///
/// Declared rather than derived. Automatic derivation from system parameters is
/// silently incomplete for writes that go through the deposit buffer, and a
/// graph that quietly omits an `ELEVATION` writer is worse than no graph — see
/// S21's resolved open question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    /// Reads the field.
    Read,
    /// Writes it directly.
    Write,
    /// Writes it through the deposit buffer, applied in `FieldDeposit`.
    Deposit,
}

impl Access {
    /// Lowercase name, as it appears in the exported graph.
    pub const fn name(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
            Access::Deposit => "deposit",
        }
    }

    /// Whether this access modifies the field.
    pub const fn is_write(self) -> bool {
        matches!(self, Access::Write | Access::Deposit)
    }
}

/// One system's declared access to one field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FieldAccess {
    /// The system doing the touching.
    pub system: &'static str,
    /// The field name, `SCREAMING_SNAKE`.
    pub field: &'static str,
    /// How it is touched.
    pub access: Access,
}

/// A dense field a module owns (S06).
///
/// A field belonging to a disabled module is never allocated — disabling ecology
/// genuinely frees `BIOMASS` rather than merely ceasing to step it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldDecl {
    /// `SCREAMING_SNAKE`, unique across all modules.
    pub name: &'static str,
    /// Bytes per cell, for the memory report.
    pub bytes_per_cell: usize,
    /// The module that registered it.
    pub module: ModuleId,
}

/// Collects a module's registrations.
///
/// Handed to [`Module::register`]; a module never touches the registry directly,
/// which is what keeps registration order out of the resolved result.
#[derive(Debug, Default)]
pub struct Registrar {
    pub(crate) module: Option<ModuleId>,
    pub(crate) systems: Vec<SystemDecl>,
    pub(crate) fields: Vec<FieldDecl>,
    pub(crate) accesses: Vec<FieldAccess>,
}

impl Registrar {
    /// Adds a system to a phase.
    ///
    /// The name must be unique within the module: it identifies the system in
    /// the schedule hash, in diagnostics, and in the S21 graph.
    pub fn system<M>(
        &mut self,
        phase: Phase,
        name: &'static str,
        system: impl IntoScheduleConfigs<ScheduleSystem, M> + Send + Sync + 'static,
    ) -> &mut Self
    where
        M: 'static,
    {
        let module = self.module.unwrap_or(ModuleId("<unregistered>"));
        self.systems.push(SystemDecl {
            name,
            phase,
            module,
            install: Box::new(move |schedule: &mut SimSchedule| {
                schedule.add_system(phase, system);
            }),
        });
        self
    }

    /// Declares how one of this module's systems touches a field.
    ///
    /// The declaration is the claim the S21 graph renders; a test cross-checks it
    /// against `bevy_ecs` access metadata where that metadata can attribute an
    /// access, which is what stops the claim from rotting.
    pub fn access(
        &mut self,
        system: &'static str,
        field: &'static str,
        access: Access,
    ) -> &mut Self {
        self.accesses.push(FieldAccess {
            system,
            field,
            access,
        });
        self
    }

    /// Declares a dense field this module owns.
    pub fn field(&mut self, name: &'static str, bytes_per_cell: usize) -> &mut Self {
        let module = self.module.unwrap_or(ModuleId("<unregistered>"));
        self.fields.push(FieldDecl {
            name,
            bytes_per_cell,
            module,
        });
        self
    }
}
