use wgpu_bridge::{
    make_buffer, make_pipeline, make_texture, open, read_rgba, release, upload_buffer,
};

#[test]
fn validation_failure_preserves_device_and_output() -> Result<(), String> {
    let mut device = open()?;
    let buffer = make_buffer(&mut device, 16, true)?;
    assert!(upload_buffer(&mut device, buffer, 16, &[0; 4]).is_err());
    upload_buffer(&mut device, buffer, 0, &[0; 4])?;

    let target = make_texture(&mut device, 3, 2, false)?;
    assert!(make_pipeline(&mut device, target, "invalid WGSL", false).is_err());

    assert!(wgpu_bridge::upload_texture(&mut device, target, 0, 0, 3, 2, &[0; 23]).is_err());
    wgpu_bridge::upload_texture(&mut device, target, 0, 0, 3, 2, &[17; 24])?;
    let mut roundtrip = Vec::new();
    read_rgba(&mut device, target, &mut roundtrip)?;
    assert_eq!(roundtrip, [17; 24]);

    let mut bytes = vec![42];
    assert!(read_rgba(&mut device, buffer, &mut bytes).is_err());
    assert_eq!(bytes, [42]);

    release(&mut device, buffer)?;
    assert!(upload_buffer(&mut device, buffer, 0, &[0; 4]).is_err());
    assert!(release(&mut device, buffer).is_err());
    Ok(())
}
