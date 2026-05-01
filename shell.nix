with import <nixpkgs> {};
mkShell {
  buildInputs = [
    gdb # required for rust-gdb
    openssl     # generate keys
    pkg-config  # required for serialport crate
    protobuf
    (python3.withPackages (p: with p; [
      ipython
      python-lsp-server
      scapy
    ]))
    rustup
    rust-analyzer
    udev  # required for serialport crate
  ];
}
