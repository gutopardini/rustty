fn main() {
    match rustty::npu::Device::open() {
        Ok(device) => {
            let info = &device.info;
            println!("NPU {}", info.path.display());
            println!("  driver     {}", info.name);
            println!("  {}", info.desc);
            println!("  device     {:#06x} rev {}", info.device_id, info.revision);
            println!("  clock      {} MHz", info.clock_hz / 1_000_000);
            println!("  context    {} / {}", info.context_id, info.max_contexts);
            println!("  base       {:#x}", info.context_base);
            println!("  firmware   {:#x}", info.fw_api);
            println!("  sku        {:#x}", info.sku);
            println!("  preempt    {} bytes", info.preempt_bytes);
            println!(
                "  caps       metric={} dma={} cmdq={} userptr={}",
                info.cap_metric_streamer, info.cap_dma_range, info.cap_manage_cmdq, info.cap_userptr
            );
            println!("  heartbeat  {}", info.heartbeat);
            match device.create_buffer(4096, true) {
                Ok(buffer) => println!("  bo         vpu {:#x}  {} bytes", buffer.vpu_addr, buffer.size),
                Err(err) => println!("  bo         falhou: {err}"),
            }
        }
        Err(err) => println!("NPU indisponível: {err}"),
    }

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
        apply_limit_buckets: false,
    })) {
        Ok(adapter) => {
            let info = adapter.get_info();
            println!("GPU {}", info.name);
            println!("  backend    {:?}", info.backend);
            println!("  device     {:#06x}", info.device);
            println!("  driver     {} {}", info.driver, info.driver_info);
        }
        Err(err) => println!("GPU Vulkan indisponível: {err}"),
    }
}
