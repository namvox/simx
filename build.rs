fn main() {
    println!("cargo:rerun-if-changed=native/src/simx_bridge.m");
    println!("cargo:rerun-if-changed=native/include/CoreSimulator.h");
    println!("cargo:rerun-if-changed=native/include/SimulatorKit.h");
    println!("cargo:rustc-link-search=framework=/Library/Developer/PrivateFrameworks");
    println!("cargo:rustc-link-search=framework=/Applications/Xcode.app/Contents/Developer/Library/PrivateFrameworks");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/Library/Developer/PrivateFrameworks");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/Applications/Xcode.app/Contents/Developer/Library/PrivateFrameworks");
    println!("cargo:rustc-link-arg=-Wl,-weak_framework,CoreSimulator");
    println!("cargo:rustc-link-arg=-Wl,-weak_framework,SimulatorKit");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=CoreGraphics");
    println!("cargo:rustc-link-lib=framework=CoreImage");
    println!("cargo:rustc-link-lib=framework=ImageIO");
    println!("cargo:rustc-link-lib=framework=IOSurface");

    cc::Build::new()
        .file("native/src/simx_bridge.m")
        .include("native/include")
        .flag("-fobjc-arc")
        .flag("-Wno-everything")
        .compile("simx_bridge");
}
