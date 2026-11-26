//! Dense fixed-function registers retain keys across unrelated writes. The
//! slot numbers and exact stored bits preserve existing content identities.
use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    ops::Index,
    sync::{Arc, OnceLock},
};

#[derive(Clone)]
pub(crate) struct StateMap(Arc<Contents>);
#[derive(Clone, Default)]
struct Contents {
    values: Vec<u32>,
    present: [u64; 4],
    keys: Vec<i32>,
    key: OnceLock<u128>,
}
impl std::fmt::Debug for StateMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}
impl PartialEq for StateMap {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl StateMap {
    pub(crate) fn get(&self, key: &i32) -> Option<&u32> {
        self.0
            .values
            .get(*key as usize)
            .filter(|_| self.0.present[*key as usize / 64] & (1 << (*key as usize % 64)) != 0)
    }
    fn mutable(&mut self) -> &mut Contents {
        let contents = Arc::make_mut(&mut self.0);
        contents.key.take();
        contents
    }
    #[cfg(test)]
    pub(crate) fn get_mut(&mut self, key: &i32) -> Option<&mut u32> {
        self.get(key)?;
        self.mutable().values.get_mut(*key as usize)
    }
    pub(crate) fn keys(&self) -> std::slice::Iter<'_, i32> {
        self.0.keys.iter()
    }
    fn iter(&self) -> impl Iterator<Item = (&i32, &u32)> {
        self.keys().map(|key| (key, &self.0.values[*key as usize]))
    }
    #[cfg(test)]
    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = &mut u32> {
        let contents = self.mutable();
        let present = contents.present;
        contents
            .values
            .iter_mut()
            .enumerate()
            .filter_map(move |(key, value)| {
                (present[key / 64] & (1 << (key % 64)) != 0).then_some(value)
            })
    }
    pub(crate) fn len(&self) -> usize {
        self.0.keys.len()
    }
    pub(crate) fn insert(&mut self, key: i32, value: u32) -> Option<u32> {
        let old = self.get(&key).copied();
        if old == Some(value) {
            return Some(value);
        }
        let fingerprint = self.fingerprint()
            ^ old.map_or(0, |v| crate::record::key::hash((key, v)))
            ^ crate::record::key::hash((key, value));
        let contents = self.mutable();
        contents.key = OnceLock::from(fingerprint);
        let index = key as usize;
        if index >= contents.values.len() {
            contents.values.resize(index + 1, 0);
        }
        if old.is_none() {
            let at = contents.keys.binary_search(&key).unwrap_err();
            contents.keys.insert(at, key);
            contents.present[index / 64] |= 1 << (index % 64);
        }
        contents.values[index] = value;
        old
    }
    pub(crate) fn capture(&self, keys: impl ExactSizeIterator<Item = i32>) -> Self {
        if keys.len() == self.len() {
            self.clone()
        } else {
            keys.map(|i| (i, self[&i])).collect()
        }
    }
}
impl Default for StateMap {
    fn default() -> Self {
        static EMPTY: OnceLock<StateMap> = OnceLock::new();
        EMPTY.get_or_init(|| Self(Arc::default())).clone()
    }
}
impl From<BTreeMap<i32, u32>> for StateMap {
    fn from(values: BTreeMap<i32, u32>) -> Self {
        values.into_iter().collect()
    }
}
impl FromIterator<(i32, u32)> for StateMap {
    fn from_iter<T: IntoIterator<Item = (i32, u32)>>(iter: T) -> Self {
        let mut result = Self::default();
        for (key, value) in iter {
            result.insert(key, value);
        }
        result
    }
}
impl Index<&i32> for StateMap {
    type Output = u32;
    fn index(&self, key: &i32) -> &u32 {
        self.get(key).expect("captured fixed state")
    }
}
impl StateMap {
    fn fingerprint(&self) -> u128 {
        let key = *self.0.key.get_or_init(|| self.recompute());
        #[cfg(any(test, feature = "d3d9-counters"))]
        debug_assert_eq!(key, self.recompute());
        key
    }
    fn recompute(&self) -> u128 {
        self.iter().fold(0, |key, (slot, value)| {
            key ^ crate::record::key::hash((slot, value))
        })
    }
}
impl Hash for StateMap {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.fingerprint().hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    // spec: GFX-REC-2, GFX-SHADER-4
    #[test]
    fn captures_share_keys_and_mutations_detach_only_the_changed_category() {
        let mut state = StateMap::from(BTreeMap::from([(1, 0), (2, 0x8000_0000)]));
        let saved = state.capture(BTreeSet::from([1, 2]).into_iter());
        let before = crate::record::key::hash(&saved);
        assert!(Arc::ptr_eq(&state.0, &saved.0));
        let cached = state.0.key.get().copied();
        state.insert(2, 0x8000_0000);
        assert!(Arc::ptr_eq(&state.0, &saved.0));
        assert_eq!(state.0.key.get().copied(), cached);
        state.insert(2, 0);
        assert_ne!(crate::record::key::hash(&state), before);
        assert_eq!(crate::record::key::hash(&saved), before);
        assert_eq!(saved[&2], 0x8000_0000);
        let subset = saved.capture(BTreeSet::from([1]).into_iter());
        assert_eq!(
            subset
                .iter()
                .map(|(&key, &value)| (key, value))
                .collect::<BTreeMap<_, _>>(),
            BTreeMap::from([(1, 0)])
        );
    }
}

macro_rules! register_slots {
    ($name:ident { $($slot:ident = $index:literal),+ $(,)? }) => {
        #[derive(Clone, Copy)]
        pub(crate) enum $name {
            $($slot = $index),+
        }

        impl Index<&$name> for StateMap {
            type Output = u32;

            fn index(&self, slot: &$name) -> &u32 {
                &self[&(*slot as i32)]
            }
        }
    };
}

register_slots!(Render {
    DepthEnable = 7,
    FillMode = 8,
    ShadeMode = 9,
    DepthWrite = 14,
    AlphaTest = 15,
    LastPixel = 16,
    SourceBlend = 19,
    DestinationBlend = 20,
    CullMode = 22,
    DepthCompare = 23,
    AlphaReference = 24,
    AlphaCompare = 25,
    DitherEnable = 26,
    BlendEnable = 27,
    FogEnable = 28,
    SpecularEnable = 29,
    FogColor = 34,
    TableFog = 35,
    FogStart = 36,
    FogEnd = 37,
    FogDensity = 38,
    RangeFog = 48,
    StencilEnable = 52,
    StencilFail = 53,
    StencilDepthFail = 54,
    StencilPass = 55,
    StencilCompare = 56,
    StencilReference = 57,
    StencilReadMask = 58,
    StencilWriteMask = 59,
    TextureFactor = 60,
    Wrap0 = 128,
    Wrap1 = 129,
    Wrap2 = 130,
    Wrap3 = 131,
    Wrap4 = 132,
    Wrap5 = 133,
    Wrap6 = 134,
    Wrap7 = 135,
    Clipping = 136,
    Lighting = 137,
    Ambient = 139,
    VertexFog = 140,
    ColorVertex = 141,
    LocalViewer = 142,
    NormalizeNormals = 143,
    DiffuseSource = 145,
    SpecularSource = 146,
    AmbientSource = 147,
    EmissiveSource = 148,
    VertexBlend = 151,
    ClipPlaneEnable = 152,
    PointSize = 154,
    PointSizeMin = 155,
    PointSprite = 156,
    PointScale = 157,
    PointScaleA = 158,
    PointScaleB = 159,
    PointScaleC = 160,
    MultisampleAntialias = 161,
    MultisampleMask = 162,
    DebugMonitorToken = 165,
    PointSizeMax = 166,
    IndexedVertexBlend = 167,
    ColorWrite = 168,
    TweenFactor = 170,
    BlendOperation = 171,
    PositionDegree = 172,
    NormalDegree = 173,
    ScissorEnable = 174,
    SlopeDepthBias = 175,
    AntialiasedLine = 176,
    TwoSidedStencil = 185,
    BackStencilFail = 186,
    BackStencilDepthFail = 187,
    BackStencilPass = 188,
    BackStencilCompare = 189,
    ColorWrite1 = 190,
    ColorWrite2 = 191,
    ColorWrite3 = 192,
    BlendFactor = 193,
    SrgbWrite = 194,
    DepthBias = 195,
    Wrap8 = 198,
    Wrap9 = 199,
    Wrap10 = 200,
    Wrap11 = 201,
    Wrap12 = 202,
    Wrap13 = 203,
    Wrap14 = 204,
    Wrap15 = 205,
    SeparateAlphaBlend = 206,
    SourceBlendAlpha = 207,
    DestinationBlendAlpha = 208,
    BlendOperationAlpha = 209,
});

register_slots!(Stage {
    ColorOp = 1,
    ColorArg1 = 2,
    ColorArg2 = 3,
    AlphaOp = 4,
    AlphaArg1 = 5,
    AlphaArg2 = 6,
    Bump00 = 7,
    Bump01 = 8,
    Bump10 = 9,
    Bump11 = 10,
    CoordinateIndex = 11,
    BumpScale = 22,
    BumpOffset = 23,
    TransformFlags = 24,
});

register_slots!(Sampler {
    AddressU = 1,
    AddressV = 2,
    AddressW = 3,
    BorderColor = 4,
    MagFilter = 5,
    MinFilter = 6,
    MipFilter = 7,
    MipLodBias = 8,
    MaxMipLevel = 9,
    MaxAnisotropy = 10,
    SrgbTexture = 11,
    ElementIndex = 12,
    DisplacementOffset = 13,
});
