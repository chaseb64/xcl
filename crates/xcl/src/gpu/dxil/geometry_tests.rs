//! Geometry mode in the direct translator: a GS with per-lane loops and cuts, validated in
//! every form and its records compared on the GPU with a model (and the HLSL path's native form).

use super::kernel::{translate_geometry, KernelOptions};
use crate::gpu::dxbc::SigElement;
use crate::gpu::rdna2::encode;
use crate::gpu::recomp::{dxc, mesh_passthrough, runner, translate_geometry as hlsl_geometry, GsInterface, IO_GS_PRIMS, MESH_HEADER};

fn sig(name: &str, sv: u32, reg: u32, ty: u32) -> SigElement {
    SigElement { name: name.into(), index: 0, system_value: sv, component_type: ty, register: reg, mask: 0xF, rw_mask: 0xF, stream: 0 }
}

/// ES: nothing but the jump into the GS. GS: EXEC = this wave's primitives, each lane
/// emits `min_vertices + (primitive & 3)` vertices in a loop (EXEC narrowing per lane,
/// `s_cbranch_execz` out) with an optional cut. Vertex = (lane, k, primitive, 1.0),
/// (count, lane, lane, lane).
fn branchy_program(min_vertices: u32, cut_after: Option<u32>) -> (Vec<u8>, GsInterface) {
    let e = |n: &str, enc: &str, f: &[(&str, u64)], lit: Option<u32>| encode(n, enc, f, lit).unwrap_or_else(|x| panic!("{n}: {x}"));
    const V: u64 = 256;
    let es = e("S_SETPC_B64", "ENC_SOP1", &[("SSRC0", 6)], None);
    let mut gs: Vec<Vec<u8>> = Vec::new();
    gs.push(e("S_BFE_U32", "ENC_SOP2", &[("SDST", 0), ("SSRC0", 3), ("SSRC1", 255)], Some(0x80008)));
    gs.push(e("S_BFM_B64", "ENC_SOP2", &[("SDST", 126), ("SSRC0", 0), ("SSRC1", 128)], None));
    gs.push(e("V_MBCNT_LO_U32_B32", "ENC_VOP3", &[("VDST", 20), ("SRC0", 193), ("SRC1", 128)], None));
    gs.push(e("V_MBCNT_HI_U32_B32", "ENC_VOP3", &[("VDST", 20), ("SRC0", 193), ("SRC1", V + 20)], None));
    gs.push(e("V_AND_B32", "ENC_VOP2", &[("VDST", 21), ("SRC0", 131), ("VSRC1", 2)], None)); // prim & 3
    if min_vertices != 0 {
        gs.push(e("V_ADD_NC_U32", "ENC_VOP2", &[("VDST", 21), ("SRC0", 128 + min_vertices as u64), ("VSRC1", 21)], None));
    }
    gs.push(e("V_MAD_U32_U24", "ENC_VOP3", &[("VDST", 50), ("SRC0", V + 20), ("SRC1", 255), ("SRC2", 2)], Some(160)));
    gs.push(e("V_MOV_B32", "ENC_VOP1", &[("VDST", 52), ("SRC0", 242)], None)); // 1.0
    gs.push(e("S_MOV_B32", "ENC_SOP1", &[("SDST", 4), ("SSRC0", 128)], None));
    let top = gs.len();
    gs.push(e("V_CMP_LT_U32", "ENC_VOPC", &[("SRC0", 4), ("VSRC1", 21)], None));
    gs.push(e("S_AND_B64", "ENC_SOP2", &[("SDST", 126), ("SSRC0", 126), ("SSRC1", 106)], None));
    let execz = gs.len();
    gs.push(Vec::new()); // s_cbranch_execz end
    gs.push(e("V_MOV_B32", "ENC_VOP1", &[("VDST", 51), ("SRC0", 4)], None));
    gs.push(e("DS_WRITE2_B32", "ENC_DS", &[("ADDR", 50), ("DATA0", 20), ("DATA1", 51), ("OFFSET0", 0), ("OFFSET1", 1)], None));
    gs.push(e("DS_WRITE2_B32", "ENC_DS", &[("ADDR", 50), ("DATA0", 2), ("DATA1", 52), ("OFFSET0", 2), ("OFFSET1", 3)], None));
    gs.push(e("DS_WRITE2_B32", "ENC_DS", &[("ADDR", 50), ("DATA0", 21), ("DATA1", 20), ("OFFSET0", 4), ("OFFSET1", 5)], None));
    gs.push(e("DS_WRITE2_B32", "ENC_DS", &[("ADDR", 50), ("DATA0", 20), ("DATA1", 20), ("OFFSET0", 6), ("OFFSET1", 7)], None));
    gs.push(e("V_ADD_NC_U32", "ENC_VOP2", &[("VDST", 50), ("SRC0", 160), ("VSRC1", 50)], None));
    gs.push(e("S_SENDMSG", "ENC_SOPP", &[("SIMM16", 0x22)], None)); // emit
    if let Some(count) = cut_after {
        gs.push(e("S_CMP_EQ_U32", "ENC_SOPC", &[("SSRC0", 4), ("SSRC1", 127 + count as u64)], None));
        gs.push(e("S_CBRANCH_SCC0", "ENC_SOPP", &[("SIMM16", 1)], None));
        gs.push(e("S_SENDMSG", "ENC_SOPP", &[("SIMM16", 0x12)], None)); // cut
    }
    gs.push(e("S_ADD_U32", "ENC_SOP2", &[("SDST", 4), ("SSRC0", 4), ("SSRC1", 129)], None));
    let back = gs.len();
    gs.push(Vec::new()); // s_branch top
    let end = gs.len();
    gs.push(e("S_SENDMSG", "ENC_SOPP", &[("SIMM16", 0x3)], None));
    gs.push(e("S_ENDPGM", "ENC_SOPP", &[], None));
    let pc = |gs: &Vec<Vec<u8>>, i: usize| gs[..i].iter().map(|p| if p.is_empty() { 4 } else { p.len() }).sum::<usize>() as i64;
    let br = |gs: &Vec<Vec<u8>>, name: &str, at: usize, to: usize| {
        let rel = (pc(gs, to) - pc(gs, at) - 4) / 4;
        e(name, "ENC_SOPP", &[("SIMM16", rel as i16 as u16 as u64)], None)
    };
    gs[execz] = br(&gs, "S_CBRANCH_EXECZ", execz, end);
    gs[back] = br(&gs, "S_BRANCH", back, top);
    let g = GsInterface {
        es_inputs: vec![],
        es_outputs: vec![sig("SV_Position", 1, 0, 3)],
        outputs: vec![sig("SV_Position", 1, 0, 3), sig("TEXCOORD", 0, 1, 1)],
        max_vertex_count: min_vertices + 3,
        gs_start: Some(es.len() as u64),
    };
    let mut code = es;
    code.extend(gs.concat());
    (code, g)
}

fn opts(split: bool, windows: u32) -> KernelOptions {
    KernelOptions { wave64: true, split, mem_windows: windows, sampler_base: 0 }
}

/// Both forms, ES alone, GS alone and four memory windows translate and validate.
#[test]
fn geometry_forms_validate() {
    let (code, g) = branchy_program(2, Some(2));
    for split in [false, true] {
        translate_geometry(&code, opts(split, 1), &g).unwrap_or_else(|e| panic!("split {split}: {e}"));
        translate_geometry(&code, opts(split, 4), &g).unwrap_or_else(|e| panic!("split {split}, 4 windows: {e}"));
        let es_alone = GsInterface { gs_start: None, ..g.clone() };
        translate_geometry(&code[..4], opts(split, 1), &es_alone).unwrap_or_else(|e| panic!("ES alone: {e}"));
        let gs_alone = GsInterface { gs_start: Some(0), ..g.clone() };
        translate_geometry(&code[4..], opts(split, 1), &gs_alone).unwrap_or_else(|e| panic!("GS alone: {e}"));
    }
}

#[test]
fn single_triangle_geometry_uses_ssa() {
    const BLOCK_FUNCTION: u32 = 12;
    const INST_ALLOCA: u32 = 19;
    let (code, mut g) = branchy_program(0, None);
    for (max_vertices, allocas) in [(0, 0), (1, 0), (2, 0), (3, 0), (5, 1)] {
        g.max_vertex_count = max_vertices;
        for split in [false, true] {
            let dxil = translate_geometry(&code, opts(split, 1), &g).unwrap().dxil;
            let part = crate::gpu::shader::container_parts(&dxil).unwrap().into_iter().find(|part| part.fourcc == "DXIL").unwrap();
            let bitcode = &dxil[part.offset + 24..part.offset + part.size]; // after the program header
            let root = super::bitreader_tests::parse(bitcode);
            let function = root.find(BLOCK_FUNCTION).unwrap();
            assert_eq!(function.records_with(INST_ALLOCA).len(), allocas, "max vertices {max_vertices}, split {split}");
        }
    }
}

/// The records of `branchy_program` for `prims` primitives: (group, record dwords).
fn expected_records(g: &GsInterface, prims: usize, min_vertices: usize, cut_after: Option<usize>) -> Vec<u32> {
    let rec = g.record();
    let (rd, vd, maxv) = (rec.record_dwords(), rec.vert_dwords(), rec.max_vertices as usize);
    let groups = prims.div_ceil(21);
    let triangle_cap = g.max_vertex_count.saturating_sub(2).max(1) as usize;
    let mut out = vec![0u32; groups * rd];
    for grp in 0..groups {
        let r = &mut out[grp * rd..(grp + 1) * rd];
        let (mut vb, mut tb) = (0usize, 0usize);
        for lane in 0..21.min(prims - grp * 21) {
            let p = grp * 21 + lane;
            let n = (p & 3) + min_vertices;
            let mut triangles = 0;
            for k in 0..n {
                let v = MESH_HEADER + (vb + k) * vd;
                r[v..v + 8].copy_from_slice(&[lane as u32, k as u32, p as u32, 0x3F80_0000, n as u32, lane as u32, lane as u32, lane as u32]);
                let strip_index = k - cut_after.filter(|&cut| cut <= k).unwrap_or(0);
                if strip_index >= 2 && triangles < triangle_cap {
                    let c = (vb + k) as u32;
                    let (a, b) = if strip_index % 2 == 0 { (c - 2, c - 1) } else { (c - 1, c - 2) };
                    r[MESH_HEADER + maxv * vd + tb] = a | (b << 10) | (c << 20);
                    tb += 1;
                    triangles += 1;
                }
            }
            vb += n;
        }
        r[0] = vb as u32;
        r[1] = tb as u32;
    }
    out
}

/// The direct translation's records (GPU, mesh pass-through included) for per-lane loops,
/// cuts and a partial last group, against a model of the program, in the split form and
/// (64-lane hardware) the native one; the HLSL path's records in the same forms must agree too.
#[test]
fn gpu_geometry_direct_records() {
    if geometry_gpu_available() {
        gpu_geometry_records(2, Some(2), 5, 25, true);
    }
}

#[test]
fn gpu_geometry_single_triangle_records() {
    if geometry_gpu_available() {
        gpu_geometry_records(0, None, 3, 25, false);
        gpu_geometry_records(0, Some(2), 3, 25, false);
        gpu_geometry_records(0, Some(3), 3, 25, false);
        // Four lanes fit the group's record even when they emit more than declared.
        // The original per-lane one-triangle cap still applies.
        gpu_geometry_records(2, None, 2, 4, false);
    }
}

fn geometry_gpu_available() -> bool {
    if runner::adapter().is_err() || runner::wave64_programs_supported() != Ok(true) || dxc::location().is_err() || runner::mesh_shaders_supported() != Ok(true) {
        runner::skip_test("no GPU / DXC / mesh shaders");
        return false;
    }
    true
}

fn gpu_geometry_records(min_vertices: u32, cut_after: Option<u32>, max_vertices: u32, prims: usize, compare_hlsl: bool) {
    let (code, mut g) = branchy_program(min_vertices, cut_after);
    g.max_vertex_count = max_vertices;
    let rec = g.record();
    let msb = dxc::compile(&mesh_passthrough(&rec), "main", "ms_6_6", &[]).unwrap();
    let ps_src = "struct V { float4 pos : SV_Position; nointerpolation uint4 p0 : TEXCOORD0; };\nfloat4 main(V v) : SV_Target0 { return float4(1.0, (float)v.p0.y, 0.0, 1.0); }\n";
    let ps = dxc::compile(ps_src, "main", "ps_6_6", &[]).unwrap();
    let mut io = vec![0u32; 128];
    io[IO_GS_PRIMS] = prims as u32;
    let mem = vec![0u32; 64];
    let want = expected_records(&g, prims, min_vertices as usize, cut_after.map(|count| count as usize));
    let groups = prims.div_ceil(21) as u32;
    let check = |what: &str, got: &[u32]| {
        // the vertex / triangle slots past the counts are not written
        let rd = rec.record_dwords();
        let (vd, maxv) = (rec.vert_dwords(), rec.max_vertices as usize);
        let diff: Vec<usize> = (0..want.len())
            .filter(|&i| {
                let (grp, o) = (i / rd, i % rd);
                let r = &want[grp * rd..];
                let used = o < MESH_HEADER || (o < MESH_HEADER + maxv * vd && (o - MESH_HEADER) / vd < r[0] as usize) || (o >= MESH_HEADER + maxv * vd && o - MESH_HEADER - maxv * vd < r[1] as usize);
                used && want[i] != got[i]
            })
            .take(16)
            .collect();
        assert!(diff.is_empty(), "{what}: records differ at {diff:?}: got {:?}, want {:?}", diff.iter().map(|&i| got[i]).collect::<Vec<_>>(), diff.iter().map(|&i| want[i]).collect::<Vec<_>>());
    };
    for (split, supported) in [(true, runner::wave32_supported()), (false, runner::wave64_supported())] {
        if supported != Ok(true) {
            continue;
        }
        let direct = translate_geometry(&code, opts(split, 1), &g).unwrap().dxil;
        let (_, got) = runner::mesh_draw(&direct, &msb, &ps, groups, rec.record_dwords(), &io, &mem, 32, [0.0; 4]).unwrap();
        check(&format!("direct, max {max_vertices}, cut {cut_after:?}, split {split}"), &got);
        if compare_hlsl {
            let t = hlsl_geometry(&code, g.clone(), split, 1).unwrap();
            assert!(t.complete(), "{:?}", t.unsupported_why);
            let hlsl = dxc::compile(&t.hlsl, "main", "cs_6_6", &[]).unwrap_or_else(|e| panic!("{e}"));
            let (_, got) = runner::mesh_draw(&hlsl, &msb, &ps, groups, rec.record_dwords(), &io, &mem, 32, [0.0; 4]).unwrap();
            check(&format!("HLSL, split {split}"), &got);
        }
    }
}
