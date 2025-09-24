{
  rustPlatform,
  pkg-config,
  ffmpeg_8,
}:
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "transcoder";
  version = "0.1.0";
  src = builtins.path {
    filter = (
      path: type:
      let
        bn = baseNameOf path;
      in
      bn != "flake.nix" && bn != "flake.lock" && bn != "default.nix"
    );
    path = ./.;
  };
  cargoLock = {
    lockFile = ./Cargo.lock;
  };

  nativeBuildInputs = [
    pkg-config
    rustPlatform.bindgenHook
  ];

  buildInputs = [
    ffmpeg_8
  ];

  meta.mainProgram = "transcoder";
})
