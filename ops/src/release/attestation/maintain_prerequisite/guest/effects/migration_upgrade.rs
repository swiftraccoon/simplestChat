//! Fixed SQL fixture preparation and observations outside the maintenance epoch.

use super::super::{
    host::{
        Baseline,
        migration::{self, Generation, SourceLedger},
    },
    protocol,
};
use super::*;

pub(super) fn admitted(
    fixture: &Fixture,
    request: &protocol::Request,
    first: Option<Error>,
    baseline: &Baseline,
) -> Result<()> {
    require(
        request.is_host()
            && request.scenario == "host-migration-upgrade"
            && first.is_none()
            && policy::digest(&baseline.pg)
            && fixture.pg.as_deref() == Some(baseline.pg.as_str())
            && fixture.image == baseline.image
            && !fixture.unknown_effect
            && !fixture.create_uncertain,
    )
}

impl Fixture {
    /// Reconstruct both pre-035 role columns/checks and exact prefix1..34.
    /// Lost mutation status remains a sticky ambiguous effect; no retry follows.
    pub(in super::super) async fn host_migration_prepare(
        &mut self,
        ctx: &mut Context,
        baseline: &Baseline,
    ) -> Result<Value> {
        ctx.control.checkpoint = "host-migration-prepare";
        let result: Result<Value> = async {
            ctx.check()?;
            admitted(self, &ctx.request, ctx.control.first, baseline)?;
            let sources = SourceLedger::from_sources(&ctx.sources)?;
            self.unknown_effect = true;
            self.sql(ctx, "host-migration-prepare", &sources.preparation_sql())
                .await?;
            ctx.control.checkpoint = "host-migration-before";
            let bytes = self
                .sql(ctx, "host-migration-before", &migration::observation_sql())
                .await?;
            let observed = migration_observation(&sources, &bytes, Generation::Before035)?;
            ctx.check()?;
            self.unknown_effect = false;
            Ok(observed)
        }
        .await;
        if let Err(error) = &result {
            ctx.control.record(*error);
        }
        result
    }

    /// Independently observe actual transformed data/checks and full source36.
    /// The caller has already frozen and validated the tested operation epoch.
    pub(in super::super) async fn host_migration_observe(
        &mut self,
        ctx: &mut Context,
        baseline: &Baseline,
    ) -> Result<Value> {
        ctx.control.checkpoint = "host-migration-after";
        let result: Result<Value> = async {
            ctx.check()?;
            admitted(self, &ctx.request, ctx.control.first, baseline)?;
            let sources = SourceLedger::from_sources(&ctx.sources)?;
            let bytes = self
                .sql(ctx, "host-migration-after", &migration::observation_sql())
                .await?;
            let observed = migration_observation(&sources, &bytes, Generation::After036)?;
            ctx.check()?;
            Ok(observed)
        }
        .await;
        if let Err(error) = &result {
            ctx.control.record(*error);
        }
        result
    }
}

fn migration_observation(
    sources: &SourceLedger,
    bytes: &[u8],
    generation: Generation,
) -> Result<Value> {
    if bytes.len() > 16384 {
        return Err(Error::Bounds);
    }
    sources.observe(&protocol::decode(bytes)?, generation)
}
