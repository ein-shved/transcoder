{
  rustPlatform,
  pkg-config,
  ffmpeg_7,
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
    ffmpeg_7
  ];

  meta.mainProgram = "transcoder";
})
