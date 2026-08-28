use super::Rect;

const PARALLEL_MIN_PIXELS: usize = 1 << 20;

// this is a minor optimization for the case of popups/recordings, since its relatively expensive to display
// a frame to a terminal for no benefit if unchanged
pub(super) fn region(
    dst: &mut [u8],
    dst_width: u32,
    src: &[u8],
    src_stride: usize,
    region: Rect,
    compare: bool,
) -> bool {
    let dst_stride = dst_width as usize * 4;
    let start = region.x as usize * 4;
    let bytes = region.w as usize * 4;
    let rows = region.h as usize;
    let region_rows = &mut dst[region.y as usize * dst_stride..][..rows * dst_stride];
    let src_base = region.y as usize * src_stride;
    crate::parallel::row_bands(
        region_rows,
        dst_stride,
        rows,
        PARALLEL_MIN_PIXELS,
        |band, first, count| {
            let mut changed = !compare;
            for r in 0..count {
                let source = &src[src_base + (first + r) * src_stride + start..][..bytes];
                let target = &mut band[r * dst_stride + start..][..bytes];
                if changed {
                    row(source, target);
                } else {
                    changed = row_checked(source, target);
                }
            }
            changed
        },
        |c1, c2| c1 | c2,
    )
    .unwrap_or(false)
}

fn row(source: &[u8], target: &mut [u8]) {
    for (source, target) in source.chunks_exact(4).zip(target.chunks_exact_mut(4)) {
        target[0] = source[2];
        target[1] = source[1];
        target[2] = source[0];
        target[3] = source[3];
    }
}

fn row_checked(source: &[u8], target: &mut [u8]) -> bool {
    let mut changed = false;
    for (source, target) in source.chunks_exact(4).zip(target.chunks_exact_mut(4)) {
        let next = [source[2], source[1], source[0], source[3]];
        changed |= *target != next;
        target.copy_from_slice(&next);
    }
    changed
}
