struct Colour { key: vec2<u32>, valid: u32, touched: atomic<u32> }
struct ColourLeader { owner: atomic<u32>, cache: u32, padding: vec2<u32> }
struct Cell {
    data: vec4<u32>, colour: vec2<u32>, slot: atomic<u32>, constant_epoch: atomic<u32>,
    terms: array<vec4<i32>, 4>,
    output: array<atomic<u32>, 256>,
}
struct Verification {
    epoch: u32, count: u32, motion: vec2<i32>,
    sharp: u32, padding0: u32, colour_count: atomic<u32>, padding2: u32,
    colours: array<Colour, 4096>,
    leaders: array<ColourLeader, 4096>,
    cells: array<Cell>,
}
