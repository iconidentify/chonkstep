#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

impl Size {
    pub const fn new(w: u32, h: u32) -> Self {
        Self { w, h }
    }
}

/// Absolute ceiling for one client window dimension in physical
/// pixels. The effective limit is normally the desktop's own extent
/// (see [`client_size_limit`]); this second guard keeps a malformed
/// multi-output layout or an unattached protocol surface from turning
/// a client-controlled size into a multi-gigabyte decoration buffer.
pub const MAX_CLIENT_WINDOW_DIMENSION: u32 = 8192;

/// Largest useful client size on this desktop.
///
/// A managed window cannot expose pixels beyond the union of the
/// outputs, so accepting a larger content rectangle only increases
/// allocations and protocol work. Each axis is independently bounded
/// by the real desktop and by [`MAX_CLIENT_WINDOW_DIMENSION`]. A
/// temporarily empty output layout still permits a 1x1 safe fallback.
pub const fn client_size_limit(screen: Size) -> Size {
    Size::new(
        if screen.w == 0 {
            1
        } else if screen.w > MAX_CLIENT_WINDOW_DIMENSION {
            MAX_CLIENT_WINDOW_DIMENSION
        } else {
            screen.w
        },
        if screen.h == 0 {
            1
        } else if screen.h > MAX_CLIENT_WINDOW_DIMENSION {
            MAX_CLIENT_WINDOW_DIMENSION
        } else {
            screen.h
        },
    )
}

/// Absolute ceiling on how many `wl_subsurface` links may separate a
/// surface tree's root from its deepest leaf.
///
/// The second client-controlled quantity that reaches into the
/// compositor, and the one with the worse failure. A size becomes an
/// allocation, and a refused allocation is a `SIGABRT` that at least
/// runs the panic hook; a *depth* becomes recursion, and every
/// traversal of a surface tree — three of them inside smithay, one
/// commit is enough to enter all three — is recursive. Running out of
/// stack is not a panic: the guard page faults, the kernel delivers
/// `SIGSEGV`, and nothing runs. No `tracing::error!`, no gamma-ramp
/// restore, no IPC-socket unlink, no dockapp teardown — the supervisor
/// sees only "compositor exited abnormally".
///
/// Nothing in the protocol bounds this. `wl_subcompositor.get_subsurface`
/// rejects only self-parenting and cycles, so a client that spends a
/// few hundred thousand messages — a fraction of a second — arrives at
/// tens of thousands of frames on an 8 MiB stack.
///
/// 64 is generous by two orders of magnitude in the direction that
/// matters. Real toolkits nest in single digits: the deepest tree this
/// desktop actually runs is a video surface under a decoration under a
/// toplevel.
pub const MAX_SUBSURFACE_DEPTH: u32 = 64;

/// Whether attaching a child to a parent would push some root-to-leaf
/// path past [`MAX_SUBSURFACE_DEPTH`].
///
/// `parent_depth` is how many links already separate the parent from
/// its own root; `child_height` is how many already separate the child
/// from the deepest leaf *below* it. Both halves are needed, and the
/// second is the one that is easy to miss: `wl_subcompositor` lets a
/// client build its chain leaf-first — attach S1 under S2, then S2
/// under S3 — and every one of those calls presents a parent that has
/// no parent of its own. A bound that counted only the distance to the
/// root would read zero on every link of that construction and never
/// fire.
///
/// The sum is the length of the longest path through the new link, and
/// it is the same number for the parent and for every ancestor above
/// it, which is why one check at one link is enough to keep the whole
/// tree bounded. Saturating, because both halves are client-driven.
pub const fn subsurface_link_exceeds_depth(parent_depth: u32, child_height: u32) -> bool {
    parent_depth.saturating_add(1).saturating_add(child_height) > MAX_SUBSURFACE_DEPTH
}

/// Bounds an untrusted client size before it can reach layout or
/// raster allocation. Zero is preserved because protocol-specific
/// callers use it to mean "unspecified"; positive dimensions are
/// capped to the useful desktop limit.
pub const fn clamp_client_size(size: Size, screen: Size) -> Size {
    let limit = client_size_limit(screen);
    Size::new(
        if size.w > limit.w { limit.w } else { size.w },
        if size.h > limit.h { limit.h } else { size.h },
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub pos: Point,
    pub size: Size,
}

impl Rect {
    pub const fn new(pos: Point, size: Size) -> Self {
        Self { pos, size }
    }

    /// Whether `p` (in the same coordinate space as this rect) falls
    /// within it. Half-open: the far edge is excluded.
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.pos.x
            && p.y >= self.pos.y
            && p.x < self.pos.x + self.size.w as i32
            && p.y < self.pos.y + self.size.h as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_contains_is_half_open() {
        let r = Rect::new(Point::new(10, 10), Size::new(5, 5));
        assert!(r.contains(Point::new(10, 10)));
        assert!(r.contains(Point::new(14, 14)));
        assert!(!r.contains(Point::new(15, 15)));
        assert!(!r.contains(Point::new(9, 10)));
    }

    #[test]
    fn client_size_is_bounded_by_the_desktop_and_absolute_ceiling() {
        assert_eq!(
            clamp_client_size(Size::new(u32::MAX, 100_000), Size::new(2560, 1600)),
            Size::new(2560, 1600)
        );
        assert_eq!(
            client_size_limit(Size::new(20_000, 10_000)),
            Size::new(MAX_CLIENT_WINDOW_DIMENSION, MAX_CLIENT_WINDOW_DIMENSION)
        );
        assert_eq!(
            clamp_client_size(Size::new(640, 480), Size::new(2560, 1600)),
            Size::new(640, 480)
        );
    }

    #[test]
    fn subsurface_depth_counts_both_sides_of_a_new_link() {
        // A toolkit's real tree: two links under a toplevel.
        assert!(!subsurface_link_exceeds_depth(2, 0));
        // The last link that fits, and the first that does not.
        assert!(!subsurface_link_exceeds_depth(MAX_SUBSURFACE_DEPTH - 1, 0));
        assert!(subsurface_link_exceeds_depth(MAX_SUBSURFACE_DEPTH, 0));
        // The leaf-first construction: the parent is a fresh root every
        // time, and the whole chain hangs below the child. A bound that
        // read only `parent_depth` would pass every one of these.
        assert!(!subsurface_link_exceeds_depth(0, MAX_SUBSURFACE_DEPTH - 1));
        assert!(subsurface_link_exceeds_depth(0, MAX_SUBSURFACE_DEPTH));
        // Meeting in the middle is the same arithmetic.
        assert!(!subsurface_link_exceeds_depth(31, 32));
        assert!(subsurface_link_exceeds_depth(32, 32));
        // Neither client-supplied half can wrap the sum past the bound.
        assert!(subsurface_link_exceeds_depth(u32::MAX, u32::MAX));
    }
}
