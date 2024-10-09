use std::{os::raw::c_void, sync::Arc};

use ash::vk;
use bevy::{
    prelude::*,
    render::{
        camera::{ManualTextureView, ManualTextureViewHandle, ManualTextureViews},
        renderer::{
            RenderAdapter, RenderAdapterInfo, RenderDevice, RenderInstance, RenderQueue,
            WgpuWrapper,
        },
        settings::{RenderCreation, WgpuSettings},
        RenderPlugin,
    },
};
use gtk::gdk;
use wgpu::TextureFormat;
use wgpu_hal::{vulkan, Instance};

use crate::{hal_custom, AdwaitaPlugin};

impl AdwaitaPlugin {
    #[must_use]
    pub fn render_plugin() -> RenderPlugin {
        let render_creation = create_renderer();
        RenderPlugin {
            render_creation,
            synchronous_pipeline_compilation: false,
        }
    }
}

fn create_renderer() -> RenderCreation {
    let settings = WgpuSettings::default();

    let do_async = async move {
        let instance = unsafe {
            vulkan::Instance::init(&wgpu_hal::InstanceDescriptor {
                name: "bevy_mod_adwaita", // app name
                flags: settings.instance_flags,
                dx12_shader_compiler: settings.dx12_shader_compiler.clone(),
                gles_minor_version: settings.gles3_minor_version,
            })
        }
        .expect("failed to create vulkan instance");

        // validation works
        // let instance = unsafe { wgpu::Instance::from_hal::<vulkan::Api>(instance) };
        // let (device, queue, adapter_info, adapter) = bevy::render::renderer::initialize_renderer(
        //     &instance,
        //     &settings,
        //     &wgpu::RequestAdapterOptions {
        //         power_preference: settings.power_preference,
        //         compatible_surface: None,
        //         ..default()
        //     },
        // )
        // .await;

        // validation fails
        let adapter = unsafe { instance.enumerate_adapters() }
            .into_iter()
            .next()
            .expect("no adapters");
        let device = unsafe {
            hal_custom::open_adapter(
                &adapter.adapter,
                settings.features.clone(),
                [
                    ash::extensions::khr::GetMemoryRequirements2::name(),
                    ash::extensions::khr::ExternalMemoryFd::name(),
                ],
            )
            .expect("failed to open device")
        };
        let instance = unsafe { wgpu::Instance::from_hal::<vulkan::Api>(instance) };
        let adapter = unsafe { instance.create_adapter_from_hal(adapter) };
        let adapter_info = adapter.get_info();
        let device_descriptor =
            hal_custom::make_device_descriptor(&settings, &adapter, &adapter_info);
        let (device, queue) =
            unsafe { adapter.create_device_from_hal(device, &device_descriptor, None) }
                .expect("failed to create device");
        let device = RenderDevice::from(device);
        let queue = RenderQueue(Arc::new(WgpuWrapper::new(queue)));
        let adapter_info = RenderAdapterInfo(WgpuWrapper::new(adapter_info));
        let adapter = RenderAdapter(Arc::new(WgpuWrapper::new(adapter)));

        RenderCreation::Manual(
            device,
            queue,
            adapter_info,
            adapter,
            RenderInstance(Arc::new(WgpuWrapper::new(instance))),
        )
    };

    futures_lite::future::block_on(do_async)
}

const TEXTURE_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;

pub fn setup_render_target(
    size: UVec2,
    manual_texture_view_handle: ManualTextureViewHandle,
    manual_texture_views: &mut ManualTextureViews,
    render_device: &RenderDevice,
) -> i32 {
    let wgpu_device = render_device.wgpu_device();
    let (texture, fd) = unsafe {
        let r = wgpu_device.as_hal::<vulkan::Api, _, _>(|hal_device| {
            let hal_device = hal_device.expect("`RenderDevice` is not a vulkan device");
            let vk_device = hal_device.raw_device();
            let instance = hal_device.shared_instance().raw_instance();

            let external_memory_image_create = vk::ExternalMemoryImageCreateInfo {
                handle_types: vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
                ..default()
            };
            let image_create = vk::ImageCreateInfo {
                p_next: &external_memory_image_create as *const _ as *const c_void,
                image_type: vk::ImageType::TYPE_2D,
                format: vk::Format::R8G8B8A8_SRGB,
                extent: vk::Extent3D {
                    width: size.x,
                    height: size.y,
                    depth: 1,
                },
                mip_levels: 1,
                array_layers: 1,
                samples: vk::SampleCountFlags::TYPE_1,
                tiling: vk::ImageTiling::OPTIMAL,
                usage: vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::COLOR_ATTACHMENT,
                sharing_mode: vk::SharingMode::EXCLUSIVE,
                initial_layout: vk::ImageLayout::UNDEFINED,
                ..default()
            };
            let image = unsafe { vk_device.create_image(&image_create, None) }
                .expect("failed to create image");

            let mut memory_requirements = vk::MemoryRequirements2KHR::default();
            unsafe {
                ash::extensions::khr::GetMemoryRequirements2::new(instance, vk_device)
                    .get_image_memory_requirements2(
                        &vk::ImageMemoryRequirementsInfo2 { image, ..default() },
                        &mut memory_requirements,
                    );
            }

            let dedicated_alloc_info = vk::MemoryDedicatedAllocateInfo { image, ..default() };
            let export_info = vk::ExportMemoryAllocateInfo {
                p_next: &dedicated_alloc_info as *const _ as *const c_void,
                handle_types: vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
                ..default()
            };
            let alloc_info = vk::MemoryAllocateInfo {
                p_next: &export_info as *const _ as *const c_void,
                allocation_size: memory_requirements.memory_requirements.size,
                ..default()
            };
            let memory = unsafe { vk_device.allocate_memory(&alloc_info, None) }
                .expect("failed to allocate memory");

            let bind_image_memory = vk::BindImageMemoryInfo {
                image,
                memory,
                ..default()
            };
            unsafe { vk_device.bind_image_memory2(&[bind_image_memory]) }
                .expect("failed to bind memory to image");

            let get_memory_info = vk::MemoryGetFdInfoKHR {
                memory,
                handle_type: vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
                ..default()
            };
            let fd = unsafe {
                ash::extensions::khr::ExternalMemoryFd::new(instance, vk_device)
                    .get_memory_fd(&get_memory_info)
            }
            .expect("failed to get fd for allocated memory");

            let texture = unsafe {
                vulkan::Device::texture_from_raw(
                    image,
                    &wgpu_hal::TextureDescriptor {
                        label: Some("adwaita_render_target"),
                        size: wgpu::Extent3d {
                            width: size.x,
                            height: size.y,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba8UnormSrgb,
                        usage: wgpu_hal::TextureUses::COPY_SRC
                            | wgpu_hal::TextureUses::COLOR_TARGET,
                        memory_flags: wgpu_hal::MemoryFlags::empty(),
                        view_formats: Vec::new(),
                    },
                    None, // todo cleanup memory and image here
                )
            };

            let texture = unsafe {
                wgpu_device.create_texture_from_hal::<vulkan::Api>(
                    texture,
                    &wgpu::TextureDescriptor {
                        label: Some("adwaita_render_target"),
                        size: wgpu::Extent3d {
                            width: size.x,
                            height: size.y,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba8UnormSrgb,
                        usage: wgpu::TextureUsages::COPY_SRC
                            | wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[],
                    },
                )
            };

            (texture, fd)
        });
        r.unwrap()
    };

    let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let manual_texture_view = ManualTextureView {
        texture_view: texture_view.into(),
        size,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
    };

    manual_texture_views.insert(manual_texture_view_handle, manual_texture_view);

    fd
}

pub fn build_dmabuf_texture(size: UVec2, fd: i32) -> gdk::Texture {
    // https://docs.gtk.org/gdk4/class.DmabufTextureBuilder.html

    let builder = gdk::DmabufTextureBuilder::new();
    builder.set_width(size.x);
    builder.set_height(size.y);
    // RA24 - RGBA8888
    // https://github.com/torvalds/linux/blob/master/include/uapi/drm/drm_fourcc.h
    // https://github.com/Robin329/fourcc_code_convert/blob/master/shell/fourcc_code_convert.sh
    builder.set_fourcc(0x34324152);
    builder.set_modifier(0);

    builder.set_n_planes(1);
    // plane 0
    builder.set_fd(0, fd);
    builder.set_offset(0, 0);
    builder.set_stride(0, size.x * 4); // bytes per row

    unsafe { builder.build() }.expect("failed to build dmabuf texture")
}
