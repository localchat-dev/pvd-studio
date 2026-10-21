use super::*;

pub(super) trait TreeKey: Copy + Ord {}
impl TreeKey for u16 {}
impl TreeKey for u64 {}
impl TreeKey for usize {}
impl TreeKey for (u64, u32) {}
impl TreeKey for (u64, u16) {}
impl TreeKey for (u64, u16, u32, u64) {}

pub(super) trait Item {
    fn same_domain(&self, domain: &Domain) -> bool;
}
macro_rules! plain_items {
    ($($ty:ty),* $(,)?) => {$(
        impl Item for $ty {
            fn same_domain(&self, _domain: &Domain) -> bool { true }
        }
    )*};
}
plain_items!(u64, (), ProfileSpan, ProfileValue, MemorySample);
impl Item for Text {
    fn same_domain(&self, domain: &Domain) -> bool {
        self.0.domain().same(domain)
    }
}
impl Item for EventRef {
    fn same_domain(&self, domain: &Domain) -> bool {
        self.0.domain().same(domain)
    }
}
impl Item for ZoneData {
    fn same_domain(&self, domain: &Domain) -> bool {
        self.name.same_domain(domain) && self.events.same_domain(domain)
    }
}
