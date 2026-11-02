//! The coupling address book: "which of your ranks owns this point?"
//!
//! Each code that holds spatially distributed data (SOIL particles, FIELD
//! cells, anything else) answers this from its *own* decomposition by
//! implementing [`OwnerMap`]. GRASS never decides or moves anyone's cuts.
//! A coupler asks the receiving code's map for each record's destination and
//! hands the records to the existing routed exchange
//! (`grass_multi::RoutedRoleExchange`), which does the delivery and carries
//! the source rank back for replies (fluid force → particle owner).
//!
//! Because the contract is just points, the same coupler code serves
//! particle-mesh (particle centers), mesh-mesh (cell centers or interface
//! points) and particle-particle coupling, and works with any decomposition a
//! code chooses: even cuts, load-balanced planes, or an adaptive forest.

/// "Which of my ranks owns this point right now?"
///
/// Implemented by each code from its own decomposition. Ranks are within that
/// code's role communicator.
pub trait OwnerMap {
    /// The rank owning `point`, or `None` if no rank does (outside the
    /// domain, or a non-finite point).
    fn owner_rank(&self, point: [f64; 3]) -> Option<i32>;

    /// Changes whenever the code re-cuts (rebalancing, a moving box). A
    /// coupler that caches a map can compare versions to detect it went
    /// stale. Codes whose cuts never change can keep the default.
    fn version(&self) -> u64 {
        0
    }
}

impl<T: OwnerMap + ?Sized> OwnerMap for &T {
    fn owner_rank(&self, point: [f64; 3]) -> Option<i32> {
        (**self).owner_rank(point)
    }
    fn version(&self) -> u64 {
        (**self).version()
    }
}

impl<T: OwnerMap + ?Sized> OwnerMap for Box<T> {
    fn owner_rank(&self, point: [f64; 3]) -> Option<i32> {
        (**self).owner_rank(point)
    }
    fn version(&self) -> u64 {
        (**self).version()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two slabs split at x = 0.5, the shape of a tiny SOIL or FIELD map.
    struct Halves;
    impl OwnerMap for Halves {
        fn owner_rank(&self, p: [f64; 3]) -> Option<i32> {
            (0.0..=1.0).contains(&p[0]).then(|| (p[0] >= 0.5) as i32)
        }
    }

    fn destinations(map: &dyn OwnerMap, points: &[[f64; 3]]) -> Vec<Option<i32>> {
        points.iter().map(|p| map.owner_rank(*p)).collect()
    }

    #[test]
    fn usable_as_dyn_ref_and_box() {
        let pts = [[0.2, 0.0, 0.0], [0.7, 0.0, 0.0], [2.0, 0.0, 0.0]];
        let want = vec![Some(0), Some(1), None];
        assert_eq!(destinations(&Halves, &pts), want);
        let boxed: Box<dyn OwnerMap> = Box::new(Halves);
        assert_eq!(destinations(&boxed, &pts), want);
        assert_eq!(boxed.version(), 0);
    }
}
