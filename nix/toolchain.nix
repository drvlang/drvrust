# Pinned input-addressed toolchain slices for drvrust.
# Keeping static slices input-addressed with substitution lets remote CI workers
# fetch only the slim compiler/linker/worker (~86 MiB instead of ~443 MiB for
# full rustc-1.97.1 + gcc-15.3.0) while all user-project derivations remain
# floating content-addressed.
{ pkgs }:
let
  inherit (pkgs) lib;
  system = pkgs.stdenv.hostPlatform.system;
  rustTarget = pkgs.stdenv.hostPlatform.rust.rustcTarget;
  rustcUnwrapped = pkgs.rustc.unwrapped;
  gccUnwrapped = pkgs.stdenv.cc.cc;
  gccLib = pkgs.stdenv.cc.cc.lib;
  binutils = pkgs.binutils-unwrapped;
  glibc = pkgs.glibc;
  zlib = pkgs.zlib;

  # Slice rustc down to bin/rustc + librustc_driver + target stdlib .rlib/.rmeta/.so,
  # dropping ~876 MiB of rustc_private metadata/plugins and rustc-src.
  rustcSlim = pkgs.runCommand "drvrust-rustc-${rustcUnwrapped.version}" {
    allowSubstitutes = true;
    disallowedReferences = [ rustcUnwrapped ];
    nativeBuildInputs = [ pkgs.patchelf ];
  } ''
    mkdir -p "$out/bin" "$out/lib/rustlib/${rustTarget}/lib"
    cp "${rustcUnwrapped}/bin/rustc" "$out/bin/rustc"
    chmod u+w "$out/bin/rustc"
    patchelf --set-rpath "\$ORIGIN/../lib:${glibc}/lib" "$out/bin/rustc"
    chmod u-w "$out/bin/rustc"
    cp "${rustcUnwrapped}"/lib/librustc_driver-*.so "$out/lib/"
    for rlib in "${rustcUnwrapped}/lib/rustlib/${rustTarget}/lib"/*.rlib; do
      case "$(basename "$rlib")" in
        librustc_public*) continue ;;
      esac
      cp "$rlib" "$out/lib/rustlib/${rustTarget}/lib/"
      base="''${rlib%.rlib}"
      if [ -f "''${base}.rmeta" ]; then
        cp "''${base}.rmeta" "$out/lib/rustlib/${rustTarget}/lib/"
      fi
      if [ -f "''${base}.so" ]; then
        cp "''${base}.so" "$out/lib/rustlib/${rustTarget}/lib/"
      fi
    done
  '';

  # Minimal CRT objects + libgcc static archives needed by ld.bfd for pure-Rust linking,
  # avoiding the 264 MiB NAR (98.6 MiB compressed) gcc-15.3.0 package on remote CI jobs.
  crtSlice = pkgs.runCommand "drvrust-crt-${gccUnwrapped.version}" {
    allowSubstitutes = true;
    disallowedReferences = [ gccUnwrapped ];
  } ''
    mkdir -p "$out"
    libdir="$(dirname "$(find "${gccUnwrapped}/lib/gcc" -name crtbeginS.o | head -n 1)")"
    cp "$libdir"/crtbegin*.o "$libdir"/crtend*.o "$libdir"/libgcc.a "$libdir"/libgcc_eh.a "$out/"
  '';

  workerSrc = lib.fileset.toSource {
    root = ../src;
    fileset = lib.fileset.unions [
      ../src/main.rs
      ../src/build.rs
      ../src/linker.rs
      ../src/nixstore.rs
      ../src/plan.rs
      ../src/sha256.rs
      ../src/tar_gz.rs
      ../src/toml.rs
    ];
  };

  ldEmulation =
    if pkgs.stdenv.hostPlatform.isx86_64 then "elf_x86_64"
    else if pkgs.stdenv.hostPlatform.isAarch64 then "aarch64linux"
    else throw "drvrust: unsupported architecture ${system}";
  dynLinker =
    if pkgs.stdenv.hostPlatform.isx86_64 then "${glibc}/lib/ld-linux-x86-64.so.2"
    else if pkgs.stdenv.hostPlatform.isAarch64 then "${glibc}/lib/ld-linux-aarch64.so.1"
    else throw "drvrust: unsupported architecture ${system}";

  # Zero-crate Rust worker compiled directly with rustc + ld.bfd in ~1s.
  # Uses rustcUnwrapped/gccUnwrapped directly at build time so rustcSlim,
  # crtSlice, and worker build in parallel on cold runs while disallowedReferences
  # guarantees none of the build-time toolchains enter the runtime closure.
  worker = pkgs.runCommand "drvrust-worker" {
    allowSubstitutes = true;
    disallowedReferences = [ rustcUnwrapped rustcSlim gccUnwrapped crtSlice binutils ];
  } ''
    mkdir -p "$out/bin" "$TMPDIR/ld"
    crtdir="$(dirname "$(find "${gccUnwrapped}/lib/gcc" -name crtbeginS.o | head -n 1)")"
    cat > "$TMPDIR/ld/cc-shim" <<SH
    #!/bin/sh
    args=""
    for a in "\$@"; do
      case "\$a" in
        -Wl,*)
          rest=\''${a#-Wl,}
          IFS=,
          for part in \$rest; do args="\$args \$part"; done
          unset IFS
          ;;
        -m64|-nodefaultlibs|-nostartfiles|-pie|-B*|-fuse-ld=*|-m*) ;;
        *) args="\$args \$a" ;;
      esac
    done
    exec "${binutils}/bin/ld.bfd" \
      --eh-frame-hdr --build-id -m ${ldEmulation} -pie \
      -dynamic-linker "${dynLinker}" \
      -rpath "${glibc}/lib:${gccLib}/lib:${zlib}/lib" \
      "${glibc}/lib/Scrt1.o" "${glibc}/lib/crti.o" "$crtdir/crtbeginS.o" \
      \$args \
      -L"${glibc}/lib" -L"${gccLib}/lib" -L"$crtdir" -L"${zlib}/lib" \
      -lgcc_s -lc -lgcc \
      "$crtdir/crtendS.o" "${glibc}/lib/crtn.o"
    SH
    chmod +x "$TMPDIR/ld/cc-shim"
    "${rustcUnwrapped}/bin/rustc" --edition 2024 \
      -C opt-level=0 -C panic=abort -C strip=symbols \
      -C linker="$TMPDIR/ld/cc-shim" \
      --remap-path-prefix="${workerSrc}=/src" \
      "${workerSrc}/main.rs" -o "$out/bin/drvrust-worker"
    ln -s drvrust-worker "$out/bin/drvrust-ld"
  '';

  workerTests = pkgs.runCommand "drvrust-worker-tests" {
    __contentAddressed = true;
  } ''
    export DRVRUST_LD="${binutils}/bin/ld.bfd"
    export DRVRUST_GLIBC="${glibc}"
    export DRVRUST_GCC_LIB="${gccLib}"
    export DRVRUST_CRT="${crtSlice}"
    "${rustcSlim}/bin/rustc" --edition 2024 --test \
      -C linker="${worker}/bin/drvrust-ld" \
      -L "${zlib}/lib" \
      -C link-arg=-Wl,-rpath,"${zlib}/lib" \
      "${workerSrc}/main.rs" -o "$TMPDIR/worker-tests"
    "$TMPDIR/worker-tests"
    mkdir -p "$out"
  '';
in {
  inherit rustcSlim crtSlice worker workerTests binutils glibc gccLib;
}
