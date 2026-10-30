//! Wine 11 NLS structure offsets and globals for comparison bodies.

#[allow(non_upper_case_globals)]
mod cp {
    pub const SIZE: u32 = 0x2c;
    pub const CodePage: u32 = 0;
    pub const DefaultChar: u32 = 4;
    pub const UniDefaultChar: u32 = 6;
    pub const TransUniDefaultChar: u32 = 10;
    pub const DBCSCodePage: u32 = 12;
    pub const MultiByteTable: u32 = 28;
    pub const WideCharTable: u32 = 32;
    pub const DBCSOffsets: u32 = 40;
}
#[allow(non_upper_case_globals)]
mod kb {
    pub const locale_section: u32 = 0x871a0;
    pub const ansi_cpinfo: u32 = 0xc0b00;
    pub const oem_cpinfo: u32 = 0xc0ac0;
    pub const codepages: u32 = 0xbf480;
    pub const nb_codepages: u32 = 0xbf460;
    pub const unix_cp: u32 = 0x871b8;
}
