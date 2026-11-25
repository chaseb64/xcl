// SPDX-License-Identifier: GPL-2.0-only
#include <windows.h>
#include <d3d12.h>
#include <d3d12sdklayers.h>
#include <dxgi1_4.h>
#include <wrl/client.h>
#include <cstdio>

using Microsoft::WRL::ComPtr;

static bool report(const char* field, HRESULT result) {
    std::printf("%s=0x%08lx\n", field, static_cast<unsigned long>(result));
    return SUCCEEDED(result);
}

int main() {
    ComPtr<IDXGIFactory4> factory;
    ComPtr<IDXGIAdapter1> warp;
    DXGI_ADAPTER_DESC1 description{};
    if (!report("dxgi_factory", CreateDXGIFactory1(IID_PPV_ARGS(&factory))) ||
        !report("enum_warp", factory->EnumWarpAdapter(IID_PPV_ARGS(&warp))) ||
        !report("adapter_description", warp->GetDesc1(&description))) {
        return 1;
    }

    const bool software = (description.Flags & DXGI_ADAPTER_FLAG_SOFTWARE) != 0;
    const LUID expected = description.AdapterLuid;
    std::printf("adapter=%ls vendor=%04x device=%04x flags=0x%x software=%u dedicated_vram_bytes=%llu\n",
                description.Description, description.VendorId, description.DeviceId,
                description.Flags, static_cast<unsigned>(software),
                static_cast<unsigned long long>(description.DedicatedVideoMemory));

    ComPtr<ID3D12Device> device;
    if (!software || !report("create_device_fl12_0",
                            D3D12CreateDevice(warp.Get(), D3D_FEATURE_LEVEL_12_0, IID_PPV_ARGS(&device)))) {
        return 1;
    }
    const LUID actual = device->GetAdapterLuid();
    const bool same = expected.HighPart == actual.HighPart && expected.LowPart == actual.LowPart;
    std::printf("warp_luid=%08lx:%08lx device_luid=%08lx:%08lx match=%u\n",
                static_cast<unsigned long>(expected.HighPart), expected.LowPart,
                static_cast<unsigned long>(actual.HighPart), actual.LowPart, static_cast<unsigned>(same));

    const D3D_FEATURE_LEVEL levels[] = {D3D_FEATURE_LEVEL_12_2, D3D_FEATURE_LEVEL_12_1, D3D_FEATURE_LEVEL_12_0};
    D3D12_FEATURE_DATA_FEATURE_LEVELS feature_levels{3, levels};
    const bool feature_ok = report("feature_levels", device->CheckFeatureSupport(
        D3D12_FEATURE_FEATURE_LEVELS, &feature_levels, sizeof(feature_levels)));
    if (feature_ok) std::printf("max_feature_level=0x%x\n", feature_levels.MaxSupportedFeatureLevel);

    // Try up to shader model 6.8, including the 6.6 XCL gate.
    D3D12_FEATURE_DATA_SHADER_MODEL shader{};
    bool shader_ok = false;
    for (int model = 0x68; model >= D3D_SHADER_MODEL_6_0; --model) {
        shader.HighestShaderModel = static_cast<D3D_SHADER_MODEL>(model);
        if (SUCCEEDED(device->CheckFeatureSupport(D3D12_FEATURE_SHADER_MODEL, &shader, sizeof(shader)))) {
            std::printf("shader_model=%x.%x probe_ceiling=6.8\n",
                        shader.HighestShaderModel >> 4, shader.HighestShaderModel & 0xf);
            shader_ok = true;
            break;
        }
    }

    D3D12_FEATURE_DATA_D3D12_OPTIONS1 waves{};
    const bool waves_ok = report("options1", device->CheckFeatureSupport(
        D3D12_FEATURE_D3D12_OPTIONS1, &waves, sizeof(waves)));
    if (waves_ok) {
        std::printf("wave_ops=%d wave_lanes_min=%u wave_lanes_max=%u int64_shader_ops=%d\n",
                    waves.WaveOps, waves.WaveLaneCountMin, waves.WaveLaneCountMax, waves.Int64ShaderOps);
    }
    D3D12_FEATURE_DATA_D3D12_OPTIONS7 mesh{};
    const bool mesh_ok = report("options7", device->CheckFeatureSupport(
        D3D12_FEATURE_D3D12_OPTIONS7, &mesh, sizeof(mesh)));
    if (mesh_ok) std::printf("mesh_shader_tier=%u\n", static_cast<unsigned>(mesh.MeshShaderTier));

    ComPtr<ID3D12Debug> debug;
    report("debug_interface_availability", D3D12GetDebugInterface(IID_PPV_ARGS(&debug)));
    return same && feature_ok && shader_ok && waves_ok && mesh_ok ? 0 : 1;
}
