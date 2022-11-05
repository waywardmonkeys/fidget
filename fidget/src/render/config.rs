use crate::{
    context::{Context, Node},
    eval::EvalFamily,
    render::render2d::RenderMode,
};
use nalgebra::{
    allocator::Allocator, geometry::Transform, Const, DefaultAllocator,
    DimNameAdd, DimNameSub, DimNameSum, U1,
};
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct RenderConfig<const N: usize>
where
    nalgebra::Const<N>: nalgebra::DimNameAdd<nalgebra::U1>,
    DefaultAllocator:
        Allocator<f32, DimNameSum<Const<N>, U1>, DimNameSum<Const<N>, U1>>,
{
    pub image_size: usize,

    /// Tile sizes to use during evaluation.
    ///
    /// You'll likely want to use
    /// [`EvalFamily::tile_sizes_2d`](crate::eval::EvalFamily::tile_sizes_2d) or
    /// [`EvalFamily::tile_sizes_3d`](crate::eval::EvalFamily::tile_sizes_3d) to
    /// select this based on evaluator type.
    pub tile_sizes: Vec<usize>,
    pub threads: usize,

    pub mat: Transform<f32, nalgebra::TGeneral, N>,
}

impl<const N: usize> Default for RenderConfig<N>
where
    nalgebra::Const<N>: nalgebra::DimNameAdd<nalgebra::U1>,
    DefaultAllocator:
        Allocator<f32, DimNameSum<Const<N>, U1>, DimNameSum<Const<N>, U1>>,
{
    fn default() -> Self {
        Self {
            image_size: 512,
            tile_sizes: match N {
                2 => vec![128, 32, 8],
                _ => vec![128, 64, 32, 16, 8],
            },
            threads: 8,
            mat: Transform::identity(),
        }
    }
}

impl<const N: usize> RenderConfig<N>
where
    nalgebra::Const<N>: nalgebra::DimNameAdd<nalgebra::U1>,
    DefaultAllocator:
        Allocator<f32, DimNameSum<Const<N>, U1>, DimNameSum<Const<N>, U1>>,
    DefaultAllocator:
        nalgebra::allocator::Allocator<
            f32,
            <<Const<N> as DimNameAdd<Const<1>>>::Output as DimNameSub<
                Const<1>,
            >>::Output,
        >,
    <nalgebra::Const<N> as DimNameAdd<nalgebra::Const<1>>>::Output:
        DimNameSub<nalgebra::Const<1>>,
{
    /// Returns a modified `RenderConfig` where `mat` is adjusted based on image
    /// size, and the image size is padded to an even multiple of `tile_size`.
    pub(crate) fn align(&self) -> AlignedRenderConfig<N> {
        let mut tile_sizes: Vec<usize> = self
            .tile_sizes
            .iter()
            .skip_while(|t| **t > self.image_size)
            .cloned()
            .collect();
        if tile_sizes.is_empty() {
            tile_sizes.push(8);
        }
        // Pad image size to an even multiple of tile size.
        let image_size = (self.image_size + tile_sizes[0] - 1) / tile_sizes[0]
            * tile_sizes[0];

        // Compensate for the image size change
        let scale = image_size as f32 / self.image_size as f32;

        // Look, I'm not any happier about this than you are.
        let v = nalgebra::Vector::<
            f32,
            <<Const<N> as DimNameAdd<Const<1>>>::Output as DimNameSub<
                Const<1>,
            >>::Output,
            <DefaultAllocator as nalgebra::allocator::Allocator<
                f32,
                <<Const<N> as DimNameAdd<Const<1>>>::Output as DimNameSub<
                    Const<1>,
                >>::Output,
                U1,
            >>::Buffer,
        >::from_element(-1.0);
        let mat = self.mat.matrix()
            * nalgebra::Transform::<f32, nalgebra::TGeneral, N>::identity()
                .matrix()
                .append_scaling(2.0 / self.image_size as f32)
                .append_scaling(scale)
                .append_translation(&v);

        AlignedRenderConfig {
            image_size,
            orig_image_size: self.image_size,
            tile_sizes,
            threads: self.threads,
            mat,
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Debug)]
pub(crate) struct AlignedRenderConfig<const N: usize>
where
    nalgebra::Const<N>: nalgebra::DimNameAdd<nalgebra::U1>,
    DefaultAllocator:
        Allocator<f32, DimNameSum<Const<N>, U1>, DimNameSum<Const<N>, U1>>,
{
    pub image_size: usize,
    pub orig_image_size: usize,

    pub tile_sizes: Vec<usize>,
    pub threads: usize,

    pub mat: NPlusOneMatrix<N>,
}

/// Type for a static `f32` matrix of size `N + 1`
type NPlusOneMatrix<const N: usize> = nalgebra::Matrix<
    f32,
    <Const<N> as DimNameAdd<Const<1>>>::Output,
    <Const<N> as DimNameAdd<Const<1>>>::Output,
    <DefaultAllocator as nalgebra::allocator::Allocator<
        f32,
        <Const<N> as DimNameAdd<Const<1>>>::Output,
        <Const<N> as DimNameAdd<Const<1>>>::Output,
    >>::Buffer,
>;

impl<const N: usize> AlignedRenderConfig<N>
where
    nalgebra::Const<N>: nalgebra::DimNameAdd<nalgebra::U1>,
    DefaultAllocator:
        Allocator<f32, DimNameSum<Const<N>, U1>, DimNameSum<Const<N>, U1>>,
    <Const<N> as DimNameAdd<Const<1>>>::Output: DimNameSub<Const<1>>,
{
    #[inline]
    pub fn tile_to_offset(&self, tile: Tile<N>, x: usize, y: usize) -> usize {
        tile.offset + x + y * self.tile_sizes[0]
    }

    #[inline]
    pub fn new_tile(&self, corner: [usize; N]) -> Tile<N> {
        let x = corner[0] % self.tile_sizes[0];
        let y = corner[1] % self.tile_sizes[0];
        Tile {
            corner,
            offset: x + y * self.tile_sizes[0],
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct Tile<const N: usize> {
    pub corner: [usize; N],
    offset: usize,
}

/// Worker queue
pub struct Queue<const N: usize> {
    index: AtomicUsize,
    tiles: Vec<Tile<N>>,
}

impl<const N: usize> Queue<N> {
    pub fn new(tiles: Vec<Tile<N>>) -> Self {
        Self {
            index: AtomicUsize::new(0),
            tiles,
        }
    }
    pub fn next(&self) -> Option<Tile<N>> {
        let index = self.index.fetch_add(1, Ordering::Relaxed);
        self.tiles.get(index).cloned()
    }
}

impl RenderConfig<2> {
    /// High-level API for rendering shapes in 2D
    ///
    /// Under the hood, this delegates to
    /// [`fidget::render::render2d::render`](crate::render::render2d::render)
    pub fn run<M: RenderMode, I: EvalFamily>(
        &self,
        root: Node,
        context: Context,
    ) -> Vec<<M as RenderMode>::Output> {
        let tape = context.get_tape(root, I::REG_LIMIT);
        crate::render::render2d::render::<I, M>(tape, self)
    }
}

impl RenderConfig<3> {
    /// High-level API for rendering shapes in 2D
    ///
    /// Under the hood, this delegates to
    /// [`fidget::render::render3d::render`](crate::render::render3d::render)
    ///
    /// Returns a tuple of heightmap, RGB image.
    pub fn run<I: EvalFamily>(
        &self,
        root: Node,
        context: Context,
    ) -> (Vec<u32>, Vec<[u8; 3]>) {
        let tape = context.get_tape(root, I::REG_LIMIT);
        crate::render::render3d::render::<I>(tape, self)
    }
}

////////////////////////////////////////////////////////////////////////////////