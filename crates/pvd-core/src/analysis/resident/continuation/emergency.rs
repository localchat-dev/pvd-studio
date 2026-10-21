use super::super::owner::InlineCow;
use super::*;

pub(crate) struct EmergencyReset {
    analysis: ResidentAnalysis,
}

impl InlineCow for Continuation {}

impl EmergencyReset {
    pub(crate) fn new(domain: Domain) -> Result<Self, Error> {
        Ok(Self {
            analysis: ResidentAnalysis::new(domain)?,
        })
    }

    pub(crate) fn at(self, first_frame: usize) -> Result<ResidentAnalysis, Error> {
        let mut analysis = self.analysis;
        // This owner is never published or cloned before consumption, so a full domain needs no COW allocation.
        analysis.data.exclusive_mut()?.first_retained_frame = first_frame;
        analysis.revision = crate::analysis::ANALYSIS_REVISION.fetch_add(1, Ordering::Relaxed);
        Ok(analysis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_reset_consumption_allocates_no_cells_in_a_full_domain() {
        let domain = Domain::new(8 << 10);
        let reset = EmergencyReset::new(domain.clone()).unwrap();
        let before = domain.statistics();
        let blocked = domain.reserve(before.limit - before.logical_used).unwrap();
        let reset = reset.at(123).unwrap();
        assert!(reset.domain().same(&domain));
        assert_eq!(domain.statistics().logical_used, before.limit);
        assert_eq!(
            domain.statistics().object_allocations_total,
            before.object_allocations_total
        );
        let expected = crate::analysis::Analysis {
            first_retained_frame: 123,
            ..Default::default()
        };
        assert_eq!(
            postcard::to_stdvec(&reset).unwrap(),
            postcard::to_stdvec(&expected).unwrap()
        );
        drop((reset, blocked));
        assert_eq!(domain.statistics().logical_used, 0);
    }

    #[test]
    fn a_shared_reset_root_is_rejected_without_mutating_the_old_owner() {
        let domain = Domain::new(8 << 10);
        let reset = EmergencyReset::new(domain.clone()).unwrap();
        let held = reset.analysis.clone();
        let old = postcard::to_stdvec(&held).unwrap();
        let before = domain.statistics();
        assert!(reset.at(123).is_err());
        assert_eq!(postcard::to_stdvec(&held).unwrap(), old);
        assert_eq!(
            domain.statistics().object_allocations_total,
            before.object_allocations_total
        );
        drop(held);
        assert_eq!(domain.statistics().logical_used, 0);
    }
}
