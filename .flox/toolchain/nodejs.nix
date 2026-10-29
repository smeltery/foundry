{
  lib,
  stdenvNoCC,
  fetchurl,
  autoPatchelfHook,
  glibc,
  gcc-unwrapped,
}:
let
  version = "22.23.1";
  archives = {
    aarch64-darwin = {
      target = "darwin-arm64";
      sha256 = "fb526811860f81dcac7dd8b2b55eca4accfc5d61c3b7c2508f2639faee8a738d";
    };
    x86_64-darwin = {
      target = "darwin-x64";
      sha256 = "efeec6641a2f15f5396d27cd0b32f5062d6689d1e9e5d89607d0b29bda890233";
    };
    aarch64-linux = {
      target = "linux-arm64";
      sha256 = "0294e8b915ab75f92c7513d2fcb830ae06e10684e6c603e99a87dbf8835389c1";
    };
    x86_64-linux = {
      target = "linux-x64";
      sha256 = "9749e988f437343b7fa832c69ded82a312e41a03116d766797ac14f6f9eee578";
    };
  };
  archive = archives.${stdenvNoCC.hostPlatform.system};
in
stdenvNoCC.mkDerivation {
  pname = "nodejs";
  inherit version;

  src = fetchurl {
    url = "https://nodejs.org/dist/v${version}/node-v${version}-${archive.target}.tar.xz";
    inherit (archive) sha256;
  };

  nativeBuildInputs = lib.optionals stdenvNoCC.hostPlatform.isLinux [ autoPatchelfHook ];
  buildInputs = lib.optionals stdenvNoCC.hostPlatform.isLinux [
    glibc
    gcc-unwrapped.lib
  ];

  dontConfigure = true;
  dontBuild = true;
  dontStrip = true;
  # Darwin needs no relocation; retain the distributed binary and signature.
  dontFixup = stdenvNoCC.hostPlatform.isDarwin;

  installPhase = ''
    runHook preInstall
    mkdir -p "$out"
    cp -a . "$out/"
    runHook postInstall
  '';

  meta = {
    description = "JavaScript runtime with its distributed command-line tools";
    homepage = "https://nodejs.org";
    license = lib.licenses.mit;
    platforms = builtins.attrNames archives;
    mainProgram = "node";
  };
}
