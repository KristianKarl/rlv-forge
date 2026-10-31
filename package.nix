{ lib, stdenv, rustPlatform, pkg-config, makeWrapper, ffmpeg, zenity, runtimeLibraries }:

let
  manifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
in
rustPlatform.buildRustPackage {
  pname = manifest.package.name;
  version = manifest.package.version;

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
      ./tests
      ./data
    ];
  };

  cargoLock.lockFile = ./Cargo.lock;

  nativeBuildInputs = [ pkg-config makeWrapper ];
  buildInputs = runtimeLibraries;
  nativeCheckInputs = [ ffmpeg ];

  postFixup = ''
    wrapProgram $out/bin/rlv-forge \
      ${lib.optionalString stdenv.hostPlatform.isLinux "--prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibraries}"} \
      --prefix PATH : ${lib.makeBinPath ([ ffmpeg ] ++ lib.optionals stdenv.hostPlatform.isLinux [ zenity ])}
  '';

  meta = {
    description = manifest.package.description;
    homepage = "https://github.com/KristianKarl/rlv-forge";
    license = lib.licenses.mit;
    mainProgram = "rlv-forge";
    platforms = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
  };
}
