@group(0) @binding(0) var canvas: texture_2d<f32>;
@group(0) @binding(1) var canvas_sampler: sampler;
struct Vertex { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vertex(@builtin(vertex_index) index: u32) -> Vertex {
    var positions = array<vec2<f32>, 3>(vec2(-1.0, 1.0), vec2(3.0, 1.0), vec2(-1.0, -3.0));
    let position = positions[index];
    var result: Vertex;
    result.position = vec4(position, 0.0, 1.0);
    result.uv = (position * vec2(1.0, -1.0) + vec2(1.0)) * 0.5;
    return result;
}
@fragment fn fragment(in: Vertex) -> @location(0) vec4<f32> {
    return textureSample(canvas, canvas_sampler, in.uv);
}
