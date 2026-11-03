@vertex fn vertex_main(@builtin(vertex_index) vertex: u32,
    @builtin(instance_index) tile: u32) -> @builtin(position) vec4<f32> {
    return software_vertex(vertex, tile);
}
@fragment fn fragment_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4(software_pixel(u32(position.x), u32(position.y)).rgb, 1.0);
}
