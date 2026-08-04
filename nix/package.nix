{
  lib,
  rustPlatform,
  servedSource ? ../.,
}:

let
  manifest = builtins.fromTOML (builtins.readFile "${servedSource}/Cargo.toml");
  sourceFilter =
    path: type:
    let
      relative = lib.removePrefix "${servedSource}/" (toString path);
    in
    lib.cleanSourceFilter path type
    && (
      builtins.elem relative [
        "Cargo.toml"
        "Cargo.lock"
        "src"
        "tests"
      ]
      || lib.hasPrefix "src/" relative
      || lib.hasPrefix "tests/" relative
    );
  source = lib.cleanSourceWith {
    src = servedSource;
    filter = sourceFilter;
  };
in
rustPlatform.buildRustPackage {
  pname = "served";
  inherit (manifest.package) version;

  src = source;
  cargoLock.lockFile = "${source}/Cargo.lock";

  strictDeps = true;

  meta = {
    description = "Lightweight per-user service manager";
    homepage = "https://github.com/TunaFish2K/served";
    changelog = "https://github.com/TunaFish2K/served/releases/tag/v${manifest.package.version}";
    license = lib.licenses.unlicense;
    mainProgram = "served";
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
      "x86_64-darwin"
      "aarch64-darwin"
    ];
  };
}
