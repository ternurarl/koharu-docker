mod graph;
mod loader;
mod packages;

use std::{fmt::Display, hash::Hash, path::PathBuf};

use anyhow::Result;

use crate::{Device, Hardware};
use graph::{Component, Plan};
use packages::{Diffusion, Llama};

pub use packages::Torch;

mod sealed {
    pub trait Sealed {}
}

/// An immutable package managed by Koharu's runtime store.
#[allow(async_fn_in_trait)]
pub trait Package: sealed::Sealed + Copy + Display + Send + Sync + 'static {
    async fn install(self) -> Result<PathBuf>;
}

pub(crate) trait RuntimePackage: Package + std::fmt::Debug + Eq + Hash {
    const NAME: &'static str;

    fn dependencies(self, _hardware: &Hardware) -> Result<Vec<Component>> {
        Ok(Vec::new())
    }

    async fn activate(self) -> Result<()>;

    fn label(self) -> String {
        format!("{} {self}", Self::NAME)
    }
}

pub(crate) trait DiscoverablePackage: RuntimePackage {
    fn discover(hardware: &Hardware, preference: DevicePreference) -> Option<Self>;
}

/// Which devices a runtime plan may install packages for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevicePreference {
    /// Whatever the host offers, best candidate first.
    Any,
    /// The CPU alone, whatever accelerators the host has. The accelerator
    /// builds of some packages leave out what their CPU paths need, so a caller
    /// that runs on the CPU must not be given one of them.
    Cpu,
}

/// A process-wide runtime capability requested by a consumer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Feature {
    Torch,
    Llama,
    Diffusion,
}

/// A discovered, dependency-ordered runtime plan.
#[derive(Debug)]
pub struct Runtime {
    plan: Plan,
    hardware: Hardware,
}

impl Runtime {
    /// Plans for the best device the host offers.
    pub fn discover(features: impl IntoIterator<Item = Feature>) -> Result<Self> {
        Self::discover_with(features, DevicePreference::Any)
    }

    /// Plans for `preference`: either the host's best device or the CPU alone.
    pub fn discover_with(
        features: impl IntoIterator<Item = Feature>,
        preference: DevicePreference,
    ) -> Result<Self> {
        let features = features.into_iter().collect::<Vec<_>>();
        let hardware = Hardware::discover();
        let candidates = match preference {
            DevicePreference::Any => hardware.candidates().collect::<Vec<_>>(),
            DevicePreference::Cpu => vec![hardware.cpu_only()],
        };
        for candidate in candidates {
            if let Some(plan) = Self::plan(&features, &candidate, preference)? {
                return Ok(Self {
                    plan,
                    hardware: candidate,
                });
            }
        }
        anyhow::bail!("no device supports the requested runtime features")
    }

    fn plan(
        features: &[Feature],
        hardware: &Hardware,
        preference: DevicePreference,
    ) -> Result<Option<Plan>> {
        let mut plan = Plan::default();
        let mut previous = None;

        for feature in features {
            let Some(node) = (match feature {
                Feature::Torch => plan.require::<Torch>(hardware, preference)?,
                Feature::Llama => plan.require::<Llama>(hardware, preference)?,
                Feature::Diffusion => plan.require::<Diffusion>(hardware, preference)?,
            }) else {
                return Ok(None);
            };
            if let Some(previous) = previous
                && previous != node
            {
                plan.sequence(previous, node);
            }
            previous = Some(node);
        }
        Ok(Some(plan))
    }

    /// Installs and activates packages, then returns the process-wide device.
    #[tracing::instrument(skip_all)]
    pub async fn initialize(self) -> Result<Device> {
        let device = self.hardware.device().cloned().unwrap_or_else(Device::cpu);
        self.plan.initialize(device).await
    }
}
