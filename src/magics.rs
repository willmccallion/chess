use crate::types::Bitboard;

// Includes all generated tables: PAWN, KNIGHT, KING, ROOK, BISHOP
include!(concat!(env!("OUT_DIR"), "/generated_attacks.rs"));

struct Magic {
    mask: Bitboard,
    magic: u64,
    shift: u32,
    offset: usize,
}

const ROOK_MAGICS: [Magic; 64] = [
    Magic {
        mask: 0x101010101017e,
        magic: 0x180008028d34000,
        shift: 52,
        offset: 0,
    },
    Magic {
        mask: 0x202020202027c,
        magic: 0x2040002000100044,
        shift: 53,
        offset: 4096,
    },
    Magic {
        mask: 0x404040404047a,
        magic: 0x200208200400810,
        shift: 53,
        offset: 6144,
    },
    Magic {
        mask: 0x8080808080876,
        magic: 0x7100080500201001,
        shift: 53,
        offset: 8192,
    },
    Magic {
        mask: 0x1010101010106e,
        magic: 0xa00200810020004,
        shift: 53,
        offset: 10240,
    },
    Magic {
        mask: 0x2020202020205e,
        magic: 0x8100080204000100,
        shift: 53,
        offset: 12288,
    },
    Magic {
        mask: 0x4040404040403e,
        magic: 0x3580008022000100,
        shift: 53,
        offset: 14336,
    },
    Magic {
        mask: 0x8080808080807e,
        magic: 0x8006450004a080,
        shift: 52,
        offset: 16384,
    },
    Magic {
        mask: 0x1010101017e00,
        magic: 0xc0800020400090,
        shift: 53,
        offset: 20480,
    },
    Magic {
        mask: 0x2020202027c00,
        magic: 0x9000401000402002,
        shift: 54,
        offset: 22528,
    },
    Magic {
        mask: 0x4040404047a00,
        magic: 0x160801000200080,
        shift: 54,
        offset: 23552,
    },
    Magic {
        mask: 0x8080808087600,
        magic: 0x3001001002008,
        shift: 54,
        offset: 24576,
    },
    Magic {
        mask: 0x10101010106e00,
        magic: 0x800400080080,
        shift: 54,
        offset: 25600,
    },
    Magic {
        mask: 0x20202020205e00,
        magic: 0x800400800200,
        shift: 54,
        offset: 26624,
    },
    Magic {
        mask: 0x40404040403e00,
        magic: 0x2141010002000401,
        shift: 54,
        offset: 27648,
    },
    Magic {
        mask: 0x80808080807e00,
        magic: 0x120500044600a900,
        shift: 53,
        offset: 28672,
    },
    Magic {
        mask: 0x10101017e0100,
        magic: 0x208000804000,
        shift: 53,
        offset: 30720,
    },
    Magic {
        mask: 0x20202027c0200,
        magic: 0x808020004013,
        shift: 54,
        offset: 32768,
    },
    Magic {
        mask: 0x40404047a0400,
        magic: 0xa0008020100080,
        shift: 54,
        offset: 33792,
    },
    Magic {
        mask: 0x8080808760800,
        magic: 0xb0004008004400,
        shift: 54,
        offset: 34816,
    },
    Magic {
        mask: 0x101010106e1000,
        magic: 0x4006020008211084,
        shift: 54,
        offset: 35840,
    },
    Magic {
        mask: 0x202020205e2000,
        magic: 0x3000808004000200,
        shift: 54,
        offset: 36864,
    },
    Magic {
        mask: 0x404040403e4000,
        magic: 0x40a0040090084102,
        shift: 54,
        offset: 37888,
    },
    Magic {
        mask: 0x808080807e8000,
        magic: 0x420020014806504,
        shift: 53,
        offset: 38912,
    },
    Magic {
        mask: 0x101017e010100,
        magic: 0x8000400580017080,
        shift: 53,
        offset: 40960,
    },
    Magic {
        mask: 0x202027c020200,
        magic: 0x4040200080400080,
        shift: 54,
        offset: 43008,
    },
    Magic {
        mask: 0x404047a040400,
        magic: 0x1400100480200480,
        shift: 54,
        offset: 44032,
    },
    Magic {
        mask: 0x8080876080800,
        magic: 0x5220200108940,
        shift: 54,
        offset: 45056,
    },
    Magic {
        mask: 0x1010106e101000,
        magic: 0x23000500080010,
        shift: 54,
        offset: 46080,
    },
    Magic {
        mask: 0x2020205e202000,
        magic: 0x800a008080020400,
        shift: 54,
        offset: 47104,
    },
    Magic {
        mask: 0x4040403e404000,
        magic: 0x401002100020084,
        shift: 54,
        offset: 48128,
    },
    Magic {
        mask: 0x8080807e808000,
        magic: 0x1004210200144484,
        shift: 53,
        offset: 49152,
    },
    Magic {
        mask: 0x1017e01010100,
        magic: 0x800400028800082,
        shift: 53,
        offset: 51200,
    },
    Magic {
        mask: 0x2027c02020200,
        magic: 0x21a200641401001,
        shift: 54,
        offset: 53248,
    },
    Magic {
        mask: 0x4047a04040400,
        magic: 0x84280a003801000,
        shift: 54,
        offset: 54272,
    },
    Magic {
        mask: 0x8087608080800,
        magic: 0x200100101002008,
        shift: 54,
        offset: 55296,
    },
    Magic {
        mask: 0x10106e10101000,
        magic: 0x9001000801001004,
        shift: 54,
        offset: 56320,
    },
    Magic {
        mask: 0x20205e20202000,
        magic: 0x1001002803000400,
        shift: 54,
        offset: 57344,
    },
    Magic {
        mask: 0x40403e40404000,
        magic: 0x104100104000208,
        shift: 54,
        offset: 58368,
    },
    Magic {
        mask: 0x80807e80808000,
        magic: 0x8000008842001914,
        shift: 53,
        offset: 59392,
    },
    Magic {
        mask: 0x17e0101010100,
        magic: 0x8000800040008020,
        shift: 53,
        offset: 61440,
    },
    Magic {
        mask: 0x27c0202020200,
        magic: 0x1a30144020004008,
        shift: 54,
        offset: 63488,
    },
    Magic {
        mask: 0x47a0404040400,
        magic: 0x10002804002000,
        shift: 54,
        offset: 64512,
    },
    Magic {
        mask: 0x8760808080800,
        magic: 0x1008001000808008,
        shift: 54,
        offset: 65536,
    },
    Magic {
        mask: 0x106e1010101000,
        magic: 0x2029001008010004,
        shift: 54,
        offset: 66560,
    },
    Magic {
        mask: 0x205e2020202000,
        magic: 0x2000409020010,
        shift: 54,
        offset: 67584,
    },
    Magic {
        mask: 0x403e4040404000,
        magic: 0x90420108040010,
        shift: 54,
        offset: 68608,
    },
    Magic {
        mask: 0x807e8080808000,
        magic: 0x58810080420004,
        shift: 53,
        offset: 69632,
    },
    Magic {
        mask: 0x7e010101010100,
        magic: 0x420402102008200,
        shift: 53,
        offset: 71680,
    },
    Magic {
        mask: 0x7c020202020200,
        magic: 0x10004000200840,
        shift: 54,
        offset: 73728,
    },
    Magic {
        mask: 0x7a040404040400,
        magic: 0x8011408020120600,
        shift: 54,
        offset: 74752,
    },
    Magic {
        mask: 0x76080808080800,
        magic: 0x7800480010008280,
        shift: 54,
        offset: 75776,
    },
    Magic {
        mask: 0x6e101010101000,
        magic: 0x2248000c004200c0,
        shift: 54,
        offset: 76800,
    },
    Magic {
        mask: 0x5e202020202000,
        magic: 0x2000811040200,
        shift: 54,
        offset: 77824,
    },
    Magic {
        mask: 0x3e404040404000,
        magic: 0x8100a990400,
        shift: 54,
        offset: 78848,
    },
    Magic {
        mask: 0x7e808080808000,
        magic: 0x1000240900964200,
        shift: 53,
        offset: 79872,
    },
    Magic {
        mask: 0x7e01010101010100,
        magic: 0x8880470010208001,
        shift: 52,
        offset: 81920,
    },
    Magic {
        mask: 0x7c02020202020200,
        magic: 0x1001200802042,
        shift: 53,
        offset: 86016,
    },
    Magic {
        mask: 0x7a04040404040400,
        magic: 0x4004200010084101,
        shift: 53,
        offset: 88064,
    },
    Magic {
        mask: 0x7608080808080800,
        magic: 0x400200409001001,
        shift: 53,
        offset: 90112,
    },
    Magic {
        mask: 0x6e10101010101000,
        magic: 0x622010490200802,
        shift: 53,
        offset: 92160,
    },
    Magic {
        mask: 0x5e20202020202000,
        magic: 0x140100084204006d,
        shift: 53,
        offset: 94208,
    },
    Magic {
        mask: 0x3e40404040404000,
        magic: 0x28080090010204,
        shift: 53,
        offset: 96256,
    },
    Magic {
        mask: 0x7e80808080808000,
        magic: 0x1022044100803402,
        shift: 52,
        offset: 98304,
    },
];
const BISHOP_MAGICS: [Magic; 64] = [
    Magic {
        mask: 0x40201008040200,
        magic: 0x401020280244c448,
        shift: 58,
        offset: 0,
    },
    Magic {
        mask: 0x402010080400,
        magic: 0x4024801010e0010,
        shift: 59,
        offset: 64,
    },
    Magic {
        mask: 0x4020100a00,
        magic: 0x21012400804100,
        shift: 59,
        offset: 96,
    },
    Magic {
        mask: 0x40221400,
        magic: 0x281a01a0280280,
        shift: 59,
        offset: 128,
    },
    Magic {
        mask: 0x2442800,
        magic: 0x184042020000100,
        shift: 59,
        offset: 160,
    },
    Magic {
        mask: 0x204085000,
        magic: 0x4064120202a4400,
        shift: 59,
        offset: 192,
    },
    Magic {
        mask: 0x20408102000,
        magic: 0x20840402400002,
        shift: 59,
        offset: 224,
    },
    Magic {
        mask: 0x2040810204000,
        magic: 0x6104802802100404,
        shift: 58,
        offset: 256,
    },
    Magic {
        mask: 0x20100804020000,
        magic: 0x2040080200c200,
        shift: 59,
        offset: 320,
    },
    Magic {
        mask: 0x40201008040000,
        magic: 0x20106401006201,
        shift: 59,
        offset: 352,
    },
    Magic {
        mask: 0x4020100a0000,
        magic: 0x4490604010000,
        shift: 59,
        offset: 384,
    },
    Magic {
        mask: 0x4022140000,
        magic: 0xe01482084200100,
        shift: 59,
        offset: 416,
    },
    Magic {
        mask: 0x244280000,
        magic: 0x4140060a10020080,
        shift: 59,
        offset: 448,
    },
    Magic {
        mask: 0x20408500000,
        magic: 0x482804100000,
        shift: 59,
        offset: 480,
    },
    Magic {
        mask: 0x2040810200000,
        magic: 0x4000200900411c2,
        shift: 59,
        offset: 512,
    },
    Magic {
        mask: 0x4081020400000,
        magic: 0x8829310888010800,
        shift: 59,
        offset: 544,
    },
    Magic {
        mask: 0x10080402000200,
        magic: 0x22001020025080,
        shift: 59,
        offset: 576,
    },
    Magic {
        mask: 0x20100804000400,
        magic: 0x81004040400d400,
        shift: 59,
        offset: 608,
    },
    Magic {
        mask: 0x4020100a000a00,
        magic: 0x401080e240010,
        shift: 57,
        offset: 640,
    },
    Magic {
        mask: 0x402214001400,
        magic: 0x3004000806a8a411,
        shift: 57,
        offset: 768,
    },
    Magic {
        mask: 0x24428002800,
        magic: 0x4050201210180,
        shift: 57,
        offset: 896,
    },
    Magic {
        mask: 0x2040850005000,
        magic: 0x202000090442024,
        shift: 57,
        offset: 1024,
    },
    Magic {
        mask: 0x4081020002000,
        magic: 0x180a108080b02808,
        shift: 59,
        offset: 1152,
    },
    Magic {
        mask: 0x8102040004000,
        magic: 0x30000240202a1,
        shift: 59,
        offset: 1184,
    },
    Magic {
        mask: 0x8040200020400,
        magic: 0x1130070108081009,
        shift: 59,
        offset: 1216,
    },
    Magic {
        mask: 0x10080400040800,
        magic: 0x2500082440809,
        shift: 59,
        offset: 1248,
    },
    Magic {
        mask: 0x20100a000a1000,
        magic: 0x402a010042240400,
        shift: 57,
        offset: 1280,
    },
    Magic {
        mask: 0x40221400142200,
        magic: 0x8884004008081100,
        shift: 55,
        offset: 1408,
    },
    Magic {
        mask: 0x2442800284400,
        magic: 0xd100840204802000,
        shift: 55,
        offset: 1920,
    },
    Magic {
        mask: 0x4085000500800,
        magic: 0x8004002005e00,
        shift: 57,
        offset: 2432,
    },
    Magic {
        mask: 0x8102000201000,
        magic: 0x2040810002011000,
        shift: 59,
        offset: 2560,
    },
    Magic {
        mask: 0x10204000402000,
        magic: 0x8004030008212702,
        shift: 59,
        offset: 2592,
    },
    Magic {
        mask: 0x4020002040800,
        magic: 0x8008603002182a00,
        shift: 59,
        offset: 2624,
    },
    Magic {
        mask: 0x8040004081000,
        magic: 0x12101000030200,
        shift: 59,
        offset: 2656,
    },
    Magic {
        mask: 0x100a000a102000,
        magic: 0x8704403000020400,
        shift: 57,
        offset: 2688,
    },
    Magic {
        mask: 0x22140014224000,
        magic: 0x20400820120200,
        shift: 55,
        offset: 2816,
    },
    Magic {
        mask: 0x44280028440200,
        magic: 0x8024010400020082,
        shift: 55,
        offset: 3328,
    },
    Magic {
        mask: 0x8500050080400,
        magic: 0x120004082410091,
        shift: 57,
        offset: 3840,
    },
    Magic {
        mask: 0x10200020100800,
        magic: 0x10008220008200,
        shift: 59,
        offset: 3968,
    },
    Magic {
        mask: 0x20400040201000,
        magic: 0x829002a084208,
        shift: 59,
        offset: 4000,
    },
    Magic {
        mask: 0x2000204081000,
        magic: 0x18080884408800,
        shift: 59,
        offset: 4032,
    },
    Magic {
        mask: 0x4000408102000,
        magic: 0x184008888400420,
        shift: 59,
        offset: 4064,
    },
    Magic {
        mask: 0xa000a10204000,
        magic: 0x4010c03c8000400,
        shift: 57,
        offset: 4096,
    },
    Magic {
        mask: 0x14001422400000,
        magic: 0x4440414208000082,
        shift: 57,
        offset: 4224,
    },
    Magic {
        mask: 0x28002844020000,
        magic: 0x200040010a00a101,
        shift: 57,
        offset: 4352,
    },
    Magic {
        mask: 0x50005008040200,
        magic: 0x8100081a00204,
        shift: 57,
        offset: 4480,
    },
    Magic {
        mask: 0x20002010080400,
        magic: 0x8052161222004c18,
        shift: 59,
        offset: 4608,
    },
    Magic {
        mask: 0x40004020100800,
        magic: 0x2044048800a00,
        shift: 59,
        offset: 4640,
    },
    Magic {
        mask: 0x20408102000,
        magic: 0x400840521090c,
        shift: 59,
        offset: 4672,
    },
    Magic {
        mask: 0x40810204000,
        magic: 0x600c60205200002,
        shift: 59,
        offset: 4704,
    },
    Magic {
        mask: 0xa1020400000,
        magic: 0x105100809000a0,
        shift: 59,
        offset: 4736,
    },
    Magic {
        mask: 0x142240000000,
        magic: 0xb0004084110000,
        shift: 59,
        offset: 4768,
    },
    Magic {
        mask: 0x284402000000,
        magic: 0x2000001002021800,
        shift: 59,
        offset: 4800,
    },
    Magic {
        mask: 0x500804020000,
        magic: 0x102040408021004,
        shift: 59,
        offset: 4832,
    },
    Magic {
        mask: 0x201008040200,
        magic: 0x2788a05414005800,
        shift: 59,
        offset: 4864,
    },
    Magic {
        mask: 0x402010080400,
        magic: 0x20380881004055,
        shift: 59,
        offset: 4896,
    },
    Magic {
        mask: 0x2040810204000,
        magic: 0xb406020080880908,
        shift: 58,
        offset: 4928,
    },
    Magic {
        mask: 0x4081020400000,
        magic: 0x1090108020208,
        shift: 59,
        offset: 4992,
    },
    Magic {
        mask: 0xa102040000000,
        magic: 0x508a103a0b000,
        shift: 59,
        offset: 5024,
    },
    Magic {
        mask: 0x14224000000000,
        magic: 0x103e00041208840,
        shift: 59,
        offset: 5056,
    },
    Magic {
        mask: 0x28440200000000,
        magic: 0x802002146208200,
        shift: 59,
        offset: 5088,
    },
    Magic {
        mask: 0x50080402000000,
        magic: 0x4890481072900101,
        shift: 59,
        offset: 5120,
    },
    Magic {
        mask: 0x20100804020000,
        magic: 0x23104410244044,
        shift: 59,
        offset: 5152,
    },
    Magic {
        mask: 0x40201008040200,
        magic: 0x220049420434200,
        shift: 58,
        offset: 5184,
    },
];

#[inline(always)]
pub fn get_rook_attacks(sq: usize, occupied: Bitboard) -> Bitboard {
    let magic = &ROOK_MAGICS[sq];
    let blockers = occupied & magic.mask;
    let index = (blockers.wrapping_mul(magic.magic) >> magic.shift) as usize;
    ROOK_ATTACKS[magic.offset + index]
}

#[inline(always)]
pub fn get_bishop_attacks(sq: usize, occupied: Bitboard) -> Bitboard {
    let magic = &BISHOP_MAGICS[sq];
    let blockers = occupied & magic.mask;
    let index = (blockers.wrapping_mul(magic.magic) >> magic.shift) as usize;
    BISHOP_ATTACKS[magic.offset + index]
}

#[inline(always)]
pub fn knight_attacks_from(sq: usize) -> Bitboard {
    KNIGHT_ATTACKS[sq]
}

#[inline(always)]
pub fn king_attacks_from(sq: usize) -> Bitboard {
    KING_ATTACKS[sq]
}
