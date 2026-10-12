{
  description = "Rust chess engine development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      booksRepo = "https://github.com/official-stockfish/books/raw/master";

      perSystem =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };

          unzipTo =
            name: src:
            pkgs.runCommand name { nativeBuildInputs = [ pkgs.unzip ]; } ''
              mkdir -p $out
              unzip -d $out ${src}
            '';

          openings = pkgs.symlinkJoin {
            name = "chess-openings";
            paths = [
              (unzipTo "uho-lichess-4852" (
                pkgs.fetchurl {
                  url = "${booksRepo}/UHO_Lichess_4852_v1.epd.zip";
                  sha256 = "01qf1rjvj80bd6295iblg1g190hmwvr6hag0mdmi1ymcx08qyaaf";
                }
              ))
              (unzipTo "noob-3moves" (
                pkgs.fetchurl {
                  url = "${booksRepo}/noob_3moves.epd.zip";
                  sha256 = "1wz9ivk4p8vcmjrflfrnxpakh8bmgrwqrdpz9zjhsvc7w28jdzk8";
                }
              ))
            ];
          };

          baronBook = unzipTo "baron-polyglot-book" (
            pkgs.fetchurl {
              url = "https://maughancdn.s3.amazonaws.com/chess/The%20Baron/baronbook30.zip";
              sha256 = "0gl0acvxxrlgg653ywahc11wxkl63f3yvxwn3b6a0nxbp81p256a";
            }
          );

          stockfishNet =
            name: sha256:
            pkgs.fetchurl {
              url = "https://tests.stockfishchess.org/api/nn/${name}";
              inherit sha256;
            };

          nets = pkgs.linkFarm "stockfish-18-nets" [
            {
              name = "big.nnue";
              path = stockfishNet "nn-c288c895ea92.nnue" "19z48cxrxrspgvzmkix4lgr007rw5dmz7qwjj3m2ji4jxaawi262";
            }
            {
              name = "small.nnue";
              path = stockfishNet "nn-37f18f62d772.nnue" "17c9b9ij1jmvbxaycqxbya33q30kij4s7b3a3mz11wvjsxi8zw9p";
            }
          ];

          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        in
        {
          packages = {
            inherit openings baronBook nets;
          };

          devShell = pkgs.mkShell {
            packages = [
              rustToolchain
              pkgs.stockfish
              pkgs.perf
              (pkgs.python3.withPackages (ps: [ ps.chess ]))
            ];

            STOCKFISH = "${pkgs.stockfish}/bin/stockfish";
            CHESS_BOOK_UHO = "${openings}/UHO_Lichess_4852_v1.epd";
            CHESS_BOOK_BALANCED = "${openings}/noob_3moves.epd";
            CHESS_NNUE_BIG = "${nets}/big.nnue";
            CHESS_NNUE_SMALL = "${nets}/small.nnue";

            shellHook = ''
              mkdir -p moves
              ln -sfn ${baronBook}/baron30.bin moves/book.bin
            '';
          };
        };

      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (perSystem system));
    in
    {
      packages = forAllSystems (s: s.packages);
      devShells = forAllSystems (s: {
        default = s.devShell;
      });
    };
}
