use std::collections::HashSet;

use bstr::{BStr, ByteSlice};
use der_parser::asn1_rs::{FromBer, OptTaggedParser, ParseResult};
use der_parser::ber::{
    parse_ber_integer, parse_ber_oid, parse_ber_sequence,
    parse_ber_sequence_defined_g, parse_ber_set_of_v,
    parse_ber_tagged_explicit_g,
};
use der_parser::error::BerResult;
use der_parser::error::Error::BerValueError;
use itertools::Itertools;
#[cfg(feature = "logging")]
use log::error;
use nom::bytes::complete::{tag, take, take_till};
use nom::combinator::{cond, map, verify};
use nom::error::ErrorKind;
use nom::multi::{count, length_count};
use nom::number::complete::{be_u32, le_u32, u16, u32, u64, u8};
use nom::number::Endianness;
use nom::{Err, IResult, Parser};
use protobuf::MessageField;
use x509_parser::x509::AlgorithmIdentifier;

use crate::modules::protos;
use crate::modules::utils::asn1::SignedData;

type NomError<'a> = nom::error::Error<&'a [u8]>;

/// Mach-O magic constants
const MH_MAGIC: u32 = 0xfeedface;
const MH_CIGAM: u32 = 0xcefaedfe;
const MH_MAGIC_64: u32 = 0xfeedfacf;
const MH_CIGAM_64: u32 = 0xcffaedfe;

/// Mach-O FAT magic constants
const FAT_MAGIC: u32 = 0xcafebabe;
const FAT_CIGAM: u32 = 0xbebafeca;
const FAT_MAGIC_64: u32 = 0xcafebabf;
const FAT_CIGAM_64: u32 = 0xbfbafeca;

/// Mach-O code signature constants
const _CS_MAGIC_REQUIREMENT: u32 = 0xfade0c00;
const _CS_MAGIC_REQUIREMENTS: u32 = 0xfade0c01;
const _CS_MAGIC_CODEDIRECTORY: u32 = 0xfade0c02;
const _CS_MAGIC_EMBEDDED_SIGNATURE: u32 = 0xfade0cc0;
const _CS_MAGIC_DETACHED_SIGNATURE: u32 = 0xfade0cc1;
const CS_MAGIC_BLOBWRAPPER: u32 = 0xfade0b01;
const CS_MAGIC_EMBEDDED_ENTITLEMENTS: u32 = 0xfade7171;

/// Mach-O symtol table flag constants
const N_STAB: u8 = 0xe0; /* if any of these bits set, a symbolic debugging entry */
const _N_PEXT: u8 = 0x10; /* private external symbol bit */
const N_TYPE: u8 = 0x0e; /* mask for the type bits */
const N_EXT: u8 = 0x01; /* external symbol bit, set for external symbols */

/// Mach-o value flags for N_TYPE bits of the n_type field.
const N_UNDF: u8 = 0x0; /* undefined, n_sect == NO_SECT */
const N_ABS: u8 = 0x2; /* absolute, n_sect == NO_SECT */
const N_SECT: u8 = 0xe; /* defined in section number n_sect */
const _N_PBUD: u8 = 0xc; /* prebound undefined (defined in a dylib) */
const N_INDR: u8 = 0xa; /* indirect */

/// Mach-O export flag constants
const EXPORT_SYMBOL_FLAGS_WEAK_DEFINITION: u64 = 0x00000004;
const EXPORT_SYMBOL_FLAGS_REEXPORT: u64 = 0x00000008;
const EXPORT_SYMBOL_FLAGS_STUB_AND_RESOLVER: u64 = 0x00000010;

/// Mach-O import opcode constants
const BIND_OPCODE_MASK: u8 = 0xF0;
const BIND_IMMEDIATE_MASK: u8 = 0x0F;
const _BIND_OPCODE_DONE: u8 = 0x00;
const _BIND_OPCODE_SET_DYLIB_ORDINAL_IMM: u8 = 0x10;
const BIND_OPCODE_SET_DYLIB_ORDINAL_ULEB: u8 = 0x20;
const _BIND_OPCODE_SET_DYLIB_SPECIAL_IMM: u8 = 0x30;
const BIND_OPCODE_SET_SYMBOL_TRAILING_FLAGS_IMM: u8 = 0x40;
const _BIND_OPCODE_SET_TYPE_IMM: u8 = 0x50;
const BIND_OPCODE_SET_ADDEND_SLEB: u8 = 0x60;
const BIND_OPCODE_SET_SEGMENT_AND_OFFSET_ULEB: u8 = 0x70;
const BIND_OPCODE_ADD_ADDR_ULEB: u8 = 0x80;
const _BIND_OPCODE_DO_BIND: u8 = 0x90;
const BIND_OPCODE_DO_BIND_ADD_ADDR_ULEB: u8 = 0xA0;
const _BIND_OPCODE_DO_BIND_ADD_ADDR_IMM_SCALED: u8 = 0xB0;
const BIND_OPCODE_DO_BIND_ULEB_TIMES_SKIPPING_ULEB: u8 = 0xC0;

/// Mach-O dynamic linker constant
const LC_REQ_DYLD: u32 = 0x80000000;

/// Mach-O load commands
const LC_SEGMENT: u32 = 0x00000001;
const LC_SYMTAB: u32 = 0x00000002;
const LC_UNIXTHREAD: u32 = 0x00000005;
const LC_DYSYMTAB: u32 = 0x0000000b;
const LC_LOAD_DYLIB: u32 = 0x0000000c;
const LC_ID_DYLIB: u32 = 0x0000000d;
const LC_LOAD_DYLINKER: u32 = 0x0000000e;
const LC_ID_DYLINKER: u32 = 0x0000000f;
const LC_LOAD_WEAK_DYLIB: u32 = 0x18 | LC_REQ_DYLD;
const LC_SEGMENT_64: u32 = 0x00000019;
const LC_UUID: u32 = 0x00000001b;
const LC_RPATH: u32 = 0x1c | LC_REQ_DYLD;
const LC_CODE_SIGNATURE: u32 = 0x0000001d;
const LC_REEXPORT_DYLIB: u32 = 0x1f | LC_REQ_DYLD;
const LC_DYLD_INFO: u32 = 0x00000022;
const LC_DYLD_INFO_ONLY: u32 = 0x22 | LC_REQ_DYLD;
const LC_VERSION_MIN_MACOSX: u32 = 0x00000024;
const LC_VERSION_MIN_IPHONEOS: u32 = 0x00000025;
const LC_DYLD_ENVIRONMENT: u32 = 0x00000027;
const LC_MAIN: u32 = 0x28 | LC_REQ_DYLD;
const LC_SOURCE_VERSION: u32 = 0x0000002a;
const LC_LINKER_OPTION: u32 = 0x0000002d;
const LC_VERSION_MIN_TVOS: u32 = 0x0000002f;
const LC_VERSION_MIN_WATCHOS: u32 = 0x00000030;
const LC_BUILD_VERSION: u32 = 0x00000032;
const LC_DYLD_EXPORTS_TRIE: u32 = 0x00000033 | LC_REQ_DYLD;
const LC_DYLD_CHAINED_FIXUPS: u32 = 0x00000034 | LC_REQ_DYLD;

/// Mach-O CPU types
const CPU_TYPE_MC680X0: u32 = 0x00000006;
const CPU_TYPE_X86: u32 = 0x00000007;
const CPU_TYPE_X86_64: u32 = 0x01000007;
const CPU_TYPE_ARM: u32 = 0x0000000c;
const CPU_TYPE_ARM64: u32 = 0x0100000c;
const CPU_TYPE_MC88000: u32 = 0x0000000d;
const CPU_TYPE_SPARC: u32 = 0x0000000e;
const CPU_TYPE_POWERPC: u32 = 0x00000012;
const CPU_TYPE_POWERPC64: u32 = 0x01000012;

/// Represents a Mach-O file. It can represent both a multi-architecture
/// binary (a.k.a. FAT binary) or a single-architecture binary.
pub struct MachO<'a> {
    /// When representing a FAT binary, this contains the file magic. It's
    /// `None` when the Mach-O file is a single-architecture binary.
    fat_magic: Option<u32>,
    /// When representing a FAT binary, this array contains one entry per
    /// architecture supported by the FAT binary. In such case the number of
    /// entries in this array should be equal to the number of entries in the
    /// `files` array. When representing a single-architecture Mach-O, this
    /// array is empty.
    archs: Vec<FatArch>,
    /// This array contains an entry per architecture included in the Mach-O
    /// file. For single-architecture binaries the array contains a single
    /// entry.
    files: Vec<MachOFile<'a>>,
}

impl<'a> MachO<'a> {
    /// Given the content of Macho-O file, parses it and returns a [`MachO`]
    /// object representing the file.
    pub fn parse(data: &'a [u8]) -> Result<Self, Err<NomError<'a>>> {
        let (_, magic) = le_u32(data)?;

        if matches!(magic, FAT_MAGIC | FAT_CIGAM | FAT_MAGIC_64 | FAT_CIGAM_64)
        {
            Self::parse_fat_macho_file(data)
        } else {
            Ok(Self {
                fat_magic: None,
                archs: Vec::new(),
                files: vec![Self::parse_macho_file(data)?],
            })
        }
    }
}

impl<'a> MachO<'a> {
    /// Parses a FAT Mach-O file.
    fn parse_fat_macho_file(
        data: &'a [u8],
    ) -> Result<Self, Err<NomError<'a>>> {
        // Parse the magic number and make sure it's valid for a FAT
        // Mach-O file.
        let (remainder, magic) = verify(be_u32, |magic| {
            matches!(
                *magic,
                FAT_MAGIC | FAT_CIGAM | FAT_MAGIC_64 | FAT_CIGAM_64
            )
        })
        .parse(data)?;

        // The magic number indicates the endianness.
        let endianness = match magic {
            FAT_MAGIC | FAT_MAGIC_64 => Endianness::Big,
            FAT_CIGAM | FAT_CIGAM_64 => Endianness::Little,
            _ => unreachable!(),
        };

        // The magic number also indicates whether this is a 32-bits or
        // 64-bits binary.
        let is_32_bits = match magic {
            FAT_MAGIC | FAT_CIGAM => true,
            FAT_MAGIC_64 | FAT_CIGAM_64 => false,
            _ => unreachable!(),
        };

        // After the magic comes an u32 with the number of `fat_arch`
        // structures that follow (`fat_arch64` for 64-bits binaries). Each
        // structure describes an individual Mach-O file included in the FAT
        // binary.
        let (_, archs) = length_count(
            // number of architectures.
            u32(endianness),
            // fat_arch/fat_arch64 structure.
            map(
                (
                    u32(endianness),                    // cputype
                    u32(endianness),                    // cpusubtype
                    uint(endianness, is_32_bits),       // offset
                    uint(endianness, is_32_bits),       // size
                    u32(endianness),                    // align
                    cond(!is_32_bits, u32(endianness)), // reserved
                ),
                |(cputype, cpusubtype, offset, size, align, reserved)| {
                    FatArch {
                        cputype,
                        cpusubtype,
                        offset,
                        size,
                        align,
                        reserved: reserved.unwrap_or_default(),
                    }
                },
            ),
        )
        .parse(remainder)?;

        let mut files = Vec::new();

        // Parse each of the individual Mach-O files contained in the FAT
        // binary. Errors that occur while parsing individual Mach-O files are
        // not propagated. If the FAT file is truncated for example, we may be
        // able to parse some of the Mach-O files while the rest can't be
        // parsed, but we still consider that case a success.
        for arch in &archs {
            let start = arch.offset as usize;
            let end = start.saturating_add(arch.size as usize);

            if let Some(macho) = data.get(start..end) {
                match Self::parse_macho_file(macho) {
                    Ok(macho) => files.push(macho),
                    #[cfg(feature = "logging")]
                    Err(err) => {
                        error!("Error parsing Mach-O file: {:?}", err);
                    }
                    #[cfg(not(feature = "logging"))]
                    Err(_) => {}
                }
            };
        }

        Ok(MachO { fat_magic: Some(magic), archs, files })
    }

    /// Parses a single-architecture Mach-O file.
    fn parse_macho_file(
        data: &'a [u8],
    ) -> Result<MachOFile<'a>, Err<NomError<'a>>> {
        let (remainder, magic) = verify(be_u32, |magic| {
            matches!(*magic, MH_MAGIC | MH_CIGAM | MH_MAGIC_64 | MH_CIGAM_64)
        })
        .parse(data)?;

        let endianness = match magic {
            MH_MAGIC | MH_MAGIC_64 => Endianness::Big,
            MH_CIGAM | MH_CIGAM_64 => Endianness::Little,
            _ => unreachable!(),
        };

        let is_32_bits = match magic {
            MH_MAGIC | MH_CIGAM => true,
            MH_MAGIC_64 | MH_CIGAM_64 => false,
            _ => unreachable!(),
        };

        let (mut commands, header) = map(
            (
                u32(endianness),                    // cputype
                u32(endianness),                    // cpusubtype
                u32(endianness),                    // filetype
                u32(endianness),                    // ncmds
                u32(endianness),                    // sizeofcmds,
                u32(endianness),                    // flags,
                cond(!is_32_bits, u32(endianness)), // reserved, only in 64-bits
            ),
            |(
                cputype,
                cpusubtype,
                filetype,
                ncmds,
                sizeofcmds,
                flags,
                reserved,
            )| {
                MachOHeader {
                    magic,
                    cputype,
                    cpusubtype,
                    filetype,
                    ncmds,
                    sizeofcmds,
                    flags,
                    reserved,
                }
            },
        )
        .parse(remainder)?;

        let mut macho = MachOFile {
            endianness,
            is_32_bits,
            header,
            segments: Vec::new(),
            dylibs: Vec::new(),
            rpaths: Vec::new(),
            symtab: None,
            dysymtab: None,
            dynamic_linker: None,
            linker_options: Vec::new(),
            dyld_info: None,
            dyld_export_trie: None,
            dyld_chain_fixups: None,
            source_version: None,
            entry_point_offset: None,
            entry_point_rva: None,
            stack_size: None,
            code_signature_data: None,
            entitlements: Vec::new(),
            certificates: Vec::new(),
            uuid: None,
            build_version: None,
            min_version: None,
            exports: Vec::new(),
            imports: Vec::new(),
        };

        for _ in 0..macho.header.ncmds as usize {
            match macho.command().parse(commands) {
                Ok((c, _)) => commands = c,
                Err(err) => {
                    #[cfg(feature = "logging")]
                    error!("Error parsing Mach-O file: {:?}", err);
                    // Break the loop when the end of file has been reached.
                    // With other types of errors we keep trying to parse more
                    // commands as one individual command structure could be
                    // corrupted while the rest are ok. But when the end of
                    // the file is reached there are no more commands that can
                    // be parsed.
                    if let Err::Error(e) = err {
                        if e.code == ErrorKind::Eof {
                            break;
                        }
                    }
                }
            }
        }

        if let Some(ref symtab) = macho.symtab {
            let str_offset = symtab.stroff as usize;
            let str_end = symtab.strsize as usize;
            let sym_offset = symtab.symoff as usize;
            let nsyms = symtab.nsyms;

            // We don't want the dyld_shared_cache ones for now
            if let Some(string_table) =
                data.get(str_offset..str_offset.saturating_add(str_end))
            {
                if let Some(symbol_table) = data.get(sym_offset..) {
                    if let Err(_err) =
                        macho.parse_symtab(string_table, symbol_table, nsyms)
                    {
                        #[cfg(feature = "logging")]
                        error!("Error parsing Mach-O file: {:?}", _err);
                        // fail silently if it fails, data was not formatted
                        // correctly but parsing should still proceed for
                        // everything else
                    };
                }
            }
        }

        if let Some(entry_point_rva) = macho.entry_point_rva {
            macho.entry_point_offset = macho.rva_to_offset(entry_point_rva);
        }

        if let Some(ref code_signature_data) = macho.code_signature_data {
            let offset = code_signature_data.dataoff as usize;
            let size = code_signature_data.datasize as usize;
            if let Some(super_data) =
                data.get(offset..offset.saturating_add(size))
            {
                if let Err(_err) = macho.cs_superblob().parse(super_data) {
                    #[cfg(feature = "logging")]
                    error!("Error parsing Mach-O file: {:?}", _err);
                    // fail silently if it fails, data was not formatted
                    // correctly but parsing should still proceed for
                    // everything else
                };
            }
        }

        for (offset, size) in [
            macho
                .dyld_export_trie
                .as_ref()
                .map(|t| (t.data_off as usize, t.data_size as usize)),
            macho
                .dyld_info
                .as_ref()
                .map(|i| (i.export_off as usize, i.export_size as usize)),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(export_data) =
                data.get(offset..offset.saturating_add(size))
            {
                if let Err(_err) = macho.parse_exports(export_data) {
                    #[cfg(feature = "logging")]
                    error!("Error parsing Mach-O file: {:?}", _err);
                    // fail silently if it fails, data was not formatted
                    // correctly but parsing should still proceed for
                    // everything else
                };
            }
        }

        for (offset, size) in macho
            .dyld_info
            .as_ref()
            .map(|i| {
                [
                    (i.bind_off as usize, i.bind_size as usize),
                    (i.lazy_bind_off as usize, i.lazy_bind_size as usize),
                    (i.weak_bind_off as usize, i.weak_bind_size as usize),
                ]
            })
            .into_iter()
            .flatten()
        {
            if let Some(import_data) =
                data.get(offset..offset.saturating_add(size))
            {
                if let Err(_err) = macho.parse_imports(import_data) {
                    #[cfg(feature = "logging")]
                    error!("Error parsing Mach-O file: {:?}", _err);
                    // fail silently if it fails, data was not formatted
                    // correctly but parsing should still proceed for
                    // everything else
                };
            }
        }

        if let Some(ref chained_fixups) = macho.dyld_chain_fixups {
            let offset = chained_fixups.data_off as usize;
            let size = chained_fixups.data_size as usize;
            if let Some(fixup_data) =
                data.get(offset..offset.saturating_add(size))
            {
                if let Err(_err) = macho.parse_chained_fixups(fixup_data) {
                    #[cfg(feature = "logging")]
                    error!("Error parsing Mach-O file: {:?}", _err);
                    // fail silently if it fails, data was not formatted
                    // correctly but parsing should still proceed for
                    // everything else
                };
            }
        }

        Ok(macho)
    }
}

pub struct MachOFile<'a> {
    endianness: Endianness,
    is_32_bits: bool,
    entry_point_offset: Option<u64>,
    entry_point_rva: Option<u64>,
    stack_size: Option<u64>,
    header: MachOHeader,
    segments: Vec<Segment<'a>>,
    dylibs: Vec<Dylib<'a>>,
    symtab: Option<Symtab<'a>>,
    dysymtab: Option<Dysymtab>,
    dyld_info: Option<DyldInfo>,
    dyld_export_trie: Option<DyldExportTrie>,
    dyld_chain_fixups: Option<DyldChainFixups>,
    dynamic_linker: Option<&'a [u8]>,
    linker_options: Vec<&'a [u8]>,
    source_version: Option<String>,
    rpaths: Vec<&'a [u8]>,
    uuid: Option<&'a [u8]>,
    code_signature_data: Option<LinkedItData>,
    entitlements: Vec<String>,
    certificates: Vec<Certificate>,
    build_version: Option<BuildVersionCommand>,
    min_version: Option<MinVersion>,
    exports: Vec<String>,
    imports: Vec<String>,
}

impl MachOFile<'_> {
    /// Converts a relative virtual address (RVA) to file object.
    pub fn rva_to_offset(&self, rva: u64) -> Option<u64> {
        for segment in &self.segments {
            let start = segment.vmaddr;
            let end = segment.vmaddr.checked_add(segment.vmsize)?;
            if rva >= start && rva < end {
                return segment.fileoff.checked_add(rva.checked_sub(start)?);
            }
        }
        None
    }
}

impl<'a> MachOFile<'a> {
    /// Parser that parses a Mach-O section.
    fn section(
        &self,
    ) -> impl Parser<&'a [u8], Output = Section<'a>, Error = NomError<'a>> + '_
    {
        map(
            (
                // sectname
                map(take(16_usize), |name| {
                    BStr::new(name).trim_end_with(|c| c == '\0')
                }),
                // segname
                map(take(16_usize), |name| {
                    BStr::new(name).trim_end_with(|c| c == '\0')
                }),
                uint(self.endianness, self.is_32_bits), // addr
                uint(self.endianness, self.is_32_bits), // size
                u32(self.endianness),                   // offset
                u32(self.endianness),                   // align
                u32(self.endianness),                   // reloff
                u32(self.endianness),                   // nreloc
                u32(self.endianness),                   // flags
                u32(self.endianness),                   // reserved1
                u32(self.endianness),                   // reserved2
                cond(!self.is_32_bits, u32(self.endianness)), // reserved3
            ),
            |(
                sectname,
                segname,
                addr,
                size,
                offset,
                align,
                reloff,
                nreloc,
                flags,
                reserved1,
                reserved2,
                reserved3,
            )| {
                Section {
                    sectname,
                    segname,
                    addr,
                    size,
                    offset,
                    align,
                    reloff,
                    nreloc,
                    flags,
                    reserved1,
                    reserved2,
                    reserved3,
                }
            },
        )
    }

    /// Parser that parses a Mach-O command.
    fn command(
        &mut self,
    ) -> impl Parser<&'a [u8], Output = (), Error = NomError<'a>> + '_ {
        move |input: &'a [u8]| {
            // The first two u32 in the command are the value that indicates
            // the command type, and the size of the command's data.
            let (remainder, (command, command_size)) = (
                u32(self.endianness), // command
                u32(self.endianness), // command_size
            )
                .parse(input)?;

            // Take the command's data.
            let (remainder, command_data) = take(
                // `command_size` includes the sizes of `command` and
                // `command_size` itself, which is 8 bytes in total. So,
                // the size of the command's data is actually `command_size`
                // minus 8.
                command_size.saturating_sub(8),
            )(remainder)?;
            // Parse the command's data. Parsers for individual commands must
            // consume all `command_data`.
            match command {
                LC_MAIN => {
                    let (_, (entry_point_offset, stack_size)) =
                        self.main_command().parse(command_data)?;
                    self.entry_point_offset = Some(entry_point_offset);
                    self.stack_size = Some(stack_size);
                }
                LC_UNIXTHREAD => {
                    let (_, eip) =
                        self.thread_command().parse(command_data)?;
                    self.entry_point_rva = Some(eip);
                }
                LC_SEGMENT | LC_SEGMENT_64 => {
                    let (_, segment) =
                        self.segment_command().parse(command_data)?;
                    self.segments.push(segment);
                }
                LC_RPATH => {
                    let (_, rpath) =
                        self.rpath_command().parse(command_data)?;
                    self.rpaths.push(rpath);
                }
                LC_LOAD_DYLIB | LC_ID_DYLIB | LC_LOAD_WEAK_DYLIB
                | LC_REEXPORT_DYLIB => {
                    let (_, dylib) =
                        self.dylib_command().parse(command_data)?;
                    self.dylibs.push(dylib);
                }
                LC_SOURCE_VERSION => {
                    let (_, ver) =
                        self.source_version_command().parse(command_data)?;
                    self.source_version =
                        Some(convert_to_source_version_string(ver));
                }
                LC_ID_DYLINKER | LC_LOAD_DYLINKER | LC_DYLD_ENVIRONMENT => {
                    let (_, dylinker) =
                        self.dylinker_command().parse(command_data)?;
                    self.dynamic_linker = Some(dylinker);
                }
                LC_SYMTAB => {
                    let (_, symtab) =
                        self.symtab_command().parse(command_data)?;
                    self.symtab = Some(symtab);
                }
                LC_DYSYMTAB => {
                    let (_, dysymtab) =
                        self.dysymtab_command().parse(command_data)?;
                    self.dysymtab = Some(dysymtab);
                }
                LC_CODE_SIGNATURE => {
                    let (_, lid) =
                        self.linkeditdata_command().parse(command_data)?;
                    self.code_signature_data = Some(lid);
                }
                LC_DYLD_EXPORTS_TRIE => {
                    let (_, exports_data) =
                        self.linkeditdata_command().parse(command_data)?;
                    self.dyld_export_trie = Some(DyldExportTrie {
                        data_off: exports_data.dataoff,
                        data_size: exports_data.datasize,
                    });
                }
                LC_DYLD_CHAINED_FIXUPS => {
                    let (_, imports_data) =
                        self.linkeditdata_command().parse(command_data)?;
                    self.dyld_chain_fixups = Some(DyldChainFixups {
                        data_off: imports_data.dataoff,
                        data_size: imports_data.datasize,
                    });
                }
                LC_DYLD_INFO | LC_DYLD_INFO_ONLY => {
                    let (_, dyld_info) =
                        self.dyld_info_command().parse(command_data)?;
                    self.dyld_info = Some(dyld_info);
                }
                LC_UUID => {
                    let (_, uuid) = self.uuid_command().parse(command_data)?;
                    self.uuid = Some(uuid);
                }
                LC_BUILD_VERSION => {
                    let (_, bv) =
                        self.build_version_command().parse(command_data)?;
                    self.build_version = Some(bv);
                }
                LC_VERSION_MIN_MACOSX
                | LC_VERSION_MIN_IPHONEOS
                | LC_VERSION_MIN_TVOS
                | LC_VERSION_MIN_WATCHOS => {
                    let (_, mut mv) =
                        self.min_version_command().parse(command_data)?;
                    mv.device = command;
                    self.min_version = Some(mv);
                }
                LC_LINKER_OPTION => {
                    let (_, linker_options) =
                        self.linker_options_command().parse(command_data)?;
                    self.linker_options.extend(linker_options);
                }
                _ => {}
            }

            Ok((remainder, ()))
        }
    }

    /// Parser that parses a LC_MAIN command.
    fn main_command(
        &mut self,
    ) -> impl Parser<&'a [u8], Output = (u64, u64), Error = NomError<'a>> + '_
    {
        (
            u64(self.endianness), // entryoff,
            u64(self.endianness), // stacksize,
        )
    }

    /// Parser that parses a LC_UNIXTHREAD command.
    fn thread_command(
        &self,
    ) -> impl Parser<&'a [u8], Output = u64, Error = NomError<'a>> + '_ {
        move |input: &'a [u8]| {
            let (remainder, (_flavor, _count)) = (
                u32(self.endianness), // flavor
                u32(self.endianness), // count
            )
                .parse(input)?;

            match self.header.cputype {
                CPU_TYPE_X86 => self.x86_thread_state().parse(remainder),
                CPU_TYPE_X86_64 => self.x86_64_thread_state().parse(remainder),
                CPU_TYPE_ARM => self.arm_thread_state().parse(remainder),
                CPU_TYPE_ARM64 => self.arm64_thread_state().parse(remainder),
                CPU_TYPE_POWERPC => self.ppc_thread_state().parse(remainder),
                CPU_TYPE_POWERPC64 => {
                    self.ppc64_thread_state().parse(remainder)
                }
                CPU_TYPE_MC680X0 => self.m68k_thread_state().parse(remainder),
                CPU_TYPE_MC88000 => self.m88k_thread_state().parse(remainder),
                CPU_TYPE_SPARC => self.sparc_thread_state().parse(remainder),
                _ => Ok((remainder, 0)),
            }
        }
    }

    /// Parser that parses a LC_SEGMENT or LC_SEGMENT_64 command.
    fn segment_command(
        &self,
    ) -> impl Parser<&'a [u8], Output = Segment<'a>, Error = NomError<'a>> + '_
    {
        move |input: &'a [u8]| {
            let (
                remainder,
                (
                    segname,
                    vmaddr,
                    vmsize,
                    fileoff,
                    filesize,
                    maxprot,
                    initprot,
                    nsects,
                    flags,
                ),
            ) = (
                // name
                map(take(16_usize), |name| {
                    BStr::new(name).trim_end_with(|c| c == '\0')
                }),
                uint(self.endianness, self.is_32_bits), // vmaddr
                uint(self.endianness, self.is_32_bits), // vmsize
                uint(self.endianness, self.is_32_bits), // fileoff
                uint(self.endianness, self.is_32_bits), // filesize,
                u32(self.endianness),                   // maxprot,
                u32(self.endianness),                   // initprot,
                u32(self.endianness),                   // nsects,
                u32(self.endianness),                   // flags,
            )
                .parse(input)?;

            let (remainder, sections) =
                count(self.section(), nsects as usize).parse(remainder)?;

            Ok((
                remainder,
                Segment {
                    segname,
                    vmaddr,
                    vmsize,
                    fileoff,
                    filesize,
                    maxprot,
                    initprot,
                    nsects,
                    flags,
                    sections,
                },
            ))
        }
    }

    /// Parser that parses a LC_LOAD_DYLIB, LC_ID_DYLIB, LC_LOAD_WEAK_DYLIB
    /// or LC_REEXPORT_DYLIB command.
    fn dylib_command(
        &self,
    ) -> impl Parser<&'a [u8], Output = Dylib<'a>, Error = NomError<'a>> + '_
    {
        map(
            (
                u32(self.endianness),        // offset,
                u32(self.endianness),        // timestamp,
                u32(self.endianness),        // current_version,
                u32(self.endianness),        // compatibility_version,
                take_till(|b| b == b'\x00'), // name
            ),
            |(
                _offset,
                timestamp,
                current_version,
                compatibility_version,
                name,
            )| {
                Dylib {
                    name: BStr::new(name),
                    timestamp,
                    current_version,
                    compatibility_version,
                }
            },
        )
    }

    /// Parser that parses a LC_DYSYMTAB command.
    fn symtab_command(
        &self,
    ) -> impl Parser<&'a [u8], Output = Symtab<'a>, Error = NomError<'a>> + '_
    {
        map(
            (
                u32(self.endianness), //  symoff
                u32(self.endianness), //  nsyms
                u32(self.endianness), //  stroff
                u32(self.endianness), //  strsize
            ),
            |(symoff, nsyms, stroff, strsize)| Symtab {
                symoff,
                nsyms,
                stroff,
                strsize,
                entries: Vec::new(),
            },
        )
    }

    /// Parser that parses a LC_DYSYMTAB command.
    fn dysymtab_command(
        &self,
    ) -> impl Parser<&'a [u8], Output = Dysymtab, Error = NomError<'a>> + '_
    {
        map(
            (
                u32(self.endianness), //  ilocalsym
                u32(self.endianness), //  nlocalsym
                u32(self.endianness), //  iextdefsym
                u32(self.endianness), //  nextdefsym
                u32(self.endianness), //  tocoff
                u32(self.endianness), //  ntoc
                u32(self.endianness), //  modtaboff
                u32(self.endianness), //  nmodtab
                u32(self.endianness), //  extrefsymoff
                u32(self.endianness), //  nextrefsyms
                u32(self.endianness), //  indirectsymoff
                u32(self.endianness), //  nindirectsyms =
                u32(self.endianness), //  extreloff
                u32(self.endianness), //  nextrel
                u32(self.endianness), //  locreloff
                u32(self.endianness), //  nlocrel
            ),
            |(
                ilocalsym,
                nlocalsym,
                iextdefsym,
                nextdefsym,
                tocoff,
                ntoc,
                modtaboff,
                nmodtab,
                extrefsymoff,
                nextrefsyms,
                indirectsymoff,
                nindirectsyms,
                extreloff,
                nextrel,
                locreloff,
                nlocrel,
            )| {
                Dysymtab {
                    ilocalsym,
                    nlocalsym,
                    iextdefsym,
                    nextdefsym,
                    tocoff,
                    ntoc,
                    modtaboff,
                    nmodtab,
                    extrefsymoff,
                    nextrefsyms,
                    indirectsymoff,
                    nindirectsyms,
                    extreloff,
                    nextrel,
                    locreloff,
                    nlocrel,
                }
            },
        )
    }

    /// Parser that parses a LC_CODESIGNATURE command
    fn linkeditdata_command(
        &self,
    ) -> impl Parser<&'a [u8], Output = LinkedItData, Error = NomError<'a>> + '_
    {
        map(
            (
                u32(self.endianness), //  dataoff
                u32(self.endianness), //  datasize
            ),
            |(dataoff, datasize)| LinkedItData { dataoff, datasize },
        )
    }

    fn cs_blob(
        &self,
    ) -> impl Parser<&'a [u8], Output = CSBlob, Error = NomError<'a>> + '_
    {
        map(
            (
                u32(Endianness::Big), // magic
                u32(Endianness::Big), // length,
            ),
            |(magic, length)| CSBlob { magic, length },
        )
    }

    fn cs_index(
        &self,
    ) -> impl Parser<&'a [u8], Output = CSBlobIndex, Error = NomError<'a>> + '_
    {
        map(
            (
                u32(Endianness::Big), // blobtype
                u32(Endianness::Big), // offset,
            ),
            |(_blobtype, offset)| CSBlobIndex {
                _blobtype,
                offset,
                blob: None,
            },
        )
    }

    fn cs_superblob(
        &mut self,
    ) -> impl Parser<&'a [u8], Output = CSSuperBlob, Error = NomError<'a>> + '_
    {
        move |input: &'a [u8]| {
            let (mut remainder, (_magic, _length, count)) = (
                u32(Endianness::Big), // magic
                u32(Endianness::Big), // offset,
                u32(Endianness::Big), // count,
            )
                .parse(input)?;

            let mut super_blob =
                CSSuperBlob { _magic, _length, count, index: Vec::new() };

            let mut cs_index: CSBlobIndex;

            for _ in 0..super_blob.count {
                (remainder, cs_index) = self.cs_index().parse(remainder)?;

                cs_index.blob = input
                    .get(cs_index.offset as usize..)
                    .and_then(|blob_data| self.cs_blob().parse(blob_data).ok())
                    .map(|(_, blob)| blob);

                super_blob.index.push(cs_index);
            }

            let super_data = input;

            // Iterator over the `CSBlobIndex` entries that have some blob.
            let blobs = super_blob.index.iter().filter_map(|blob_index| {
                blob_index
                    .blob
                    .as_ref()
                    .map(|blob| (blob_index.offset as usize, blob))
            });

            for (offset, blob) in blobs {
                let length = blob.length as usize;
                let size_of_blob = std::mem::size_of::<CSBlob>();
                match blob.magic {
                    CS_MAGIC_EMBEDDED_ENTITLEMENTS => {
                        let xml_data = match super_data.get(
                            offset.saturating_add(size_of_blob)
                                ..offset.saturating_add(length),
                        ) {
                            Some(data) => data,
                            None => continue,
                        };

                        let xml_string =
                            std::str::from_utf8(xml_data).unwrap_or_default();

                        let opt = roxmltree::ParsingOptions {
                            allow_dtd: true,
                            ..roxmltree::ParsingOptions::default()
                        };

                        if let Ok(parsed_xml) =
                            roxmltree::Document::parse_with_options(
                                xml_string, opt,
                            )
                        {
                            for node in parsed_xml.descendants().filter(|n| {
                                n.has_tag_name("key")
                                    || n.has_tag_name("array")
                            }) {
                                if let Some(entitlement) = node.text() {
                                    if node.has_tag_name("array") {
                                        node.descendants()
                                            .filter_map(|n| n.text())
                                            .filter(|t| !t.trim().is_empty())
                                            .unique()
                                            .map(|t| t.to_string())
                                            .for_each(|array_entitlement| {
                                                self.entitlements
                                                    .push(array_entitlement)
                                            });
                                    } else {
                                        self.entitlements
                                            .push(entitlement.to_string());
                                    }
                                }
                            }
                        }
                    }
                    CS_MAGIC_BLOBWRAPPER => {
                        if let Some(ber_blob) = super_data.get(
                            offset.saturating_add(size_of_blob)
                                ..offset.saturating_add(length),
                        ) {
                            if let Ok((_remainder, certs)) =
                                parse_certificates(ber_blob)
                            {
                                self.certificates.extend(certs);
                            }
                        }
                    }
                    _ => {}
                }
            }

            Ok((remainder, super_blob))
        }
    }

    /// Parser that parses LC_DYLD_INFO_ONLY and LC_DYLD_INFO commands
    fn dyld_info_command(
        &self,
    ) -> impl Parser<&'a [u8], Output = DyldInfo, Error = NomError<'a>> + '_
    {
        map(
            (
                u32(self.endianness), //  rebase_off
                u32(self.endianness), //  rebase_size
                u32(self.endianness), //  bind_off
                u32(self.endianness), //  bind_size
                u32(self.endianness), //  weak_bind_off
                u32(self.endianness), //  weak_bind_size
                u32(self.endianness), //  lazy_bind_off
                u32(self.endianness), //  lazy_bind_size
                u32(self.endianness), //  export_off
                u32(self.endianness), //  export_size
            ),
            |(
                rebase_off,
                rebase_size,
                bind_off,
                bind_size,
                weak_bind_off,
                weak_bind_size,
                lazy_bind_off,
                lazy_bind_size,
                export_off,
                export_size,
            )| {
                DyldInfo {
                    rebase_off,
                    rebase_size,
                    bind_off,
                    bind_size,
                    weak_bind_off,
                    weak_bind_size,
                    lazy_bind_off,
                    lazy_bind_size,
                    export_off,
                    export_size,
                }
            },
        )
    }

    /// Parser that parses the nlist structures that are defined by the offsets
    /// in LC_SYMTAB.
    fn nlist(
        &self,
    ) -> impl Parser<&'a [u8], Output = Nlist, Error = NomError<'a>> + '_ {
        map(
            (
                u32(self.endianness),                   // n_strx
                u8,                                     // n_type
                u8,                                     // n_sect
                u16(self.endianness),                   // n_desc
                uint(self.endianness, self.is_32_bits), // n_value
            ),
            |(n_strx, n_type, n_sect, n_desc, n_value)| Nlist {
                n_strx,
                n_type,
                _n_sect: n_sect,
                _n_desc: n_desc,
                _n_value: n_value,
            },
        )
    }

    /// Parser that parses the symbol table which includes the nlist structure
    /// and the relevant string from the string table as defined in LC_SYMTAB.
    fn parse_symtab(
        &mut self,
        string_table: &'a [u8],
        symbol_table: &'a [u8],
        count: u32,
    ) -> IResult<&'a [u8], ()> {
        let mut data = symbol_table;
        let mut n;

        for _ in 0..count {
            (data, n) = self.nlist().parse(data)?;

            if let Some(symtab) = self.symtab.as_mut() {
                if let Some(string_data) =
                    string_table.get(n.n_strx as usize..)
                {
                    let (_, string_value) = map(
                        (take_till(|b| b == b'\x00'), tag("\x00")),
                        |(s, _)| s,
                    )
                    .parse(string_data)?;

                    if !string_value.is_empty() {
                        symtab.entries.push(SymbolTableEntry {
                            tags: n.n_type,
                            value: string_value,
                        });
                    }
                }
            }
        }

        Ok((data, ()))
    }

    /// Parser that parses the exports at the offsets defined within
    /// LC_DYLD_INFO, LC_DYLD_INFO_ONLY, and LC_DYLD_EXPORTS_TRIE.
    fn parse_exports(&mut self, data: &'a [u8]) -> IResult<&'a [u8], ()> {
        let mut stack = Vec::<ExportNode>::new();
        let mut visited = HashSet::<usize>::new();

        stack.push(ExportNode { offset: 0, prefix: "".to_string() });

        while !stack.is_empty() && !data.is_empty() {
            let export_node = stack.pop().unwrap();

            // If node was already visited, continue without processing it.
            if !visited.insert(export_node.offset) {
                continue;
            }

            let node_data = match data.get(export_node.offset..) {
                Some(data) => data,
                None => continue,
            };

            let (mut remaining_data, length) = uleb128(node_data)?;

            if length != 0 {
                let (remainder, flags) = uleb128(remaining_data)?;
                match flags {
                    EXPORT_SYMBOL_FLAGS_STUB_AND_RESOLVER => {
                        let (remainder, (_stub_offset, _resolver_offset)) =
                            (uleb128, uleb128).parse(remainder)?;
                        remaining_data = remainder;
                    }
                    EXPORT_SYMBOL_FLAGS_REEXPORT => {
                        let (remainder, _ordinal) = uleb128(remainder)?;
                        let (remainder, _label) = map(
                            (take_till(|b| b == b'\x00'), tag("\x00")),
                            |(s, _)| s,
                        )
                        .parse(remainder)?;

                        remaining_data = remainder;
                    }
                    EXPORT_SYMBOL_FLAGS_WEAK_DEFINITION => {
                        let (remainder, _offset) = uleb128(remainder)?;
                        remaining_data = remainder;
                    }
                    _ => {}
                }
            }

            let (mut edge_remainder, edges) = u8(remaining_data)?;

            for _ in 0..edges {
                let (remainder, edge_label) = map(
                    (take_till(|b| b == b'\x00'), tag("\x00")),
                    |(s, _)| BStr::new(s),
                )
                .parse(edge_remainder)?;

                let (remainder, edge_offset) = uleb128(remainder)?;

                if let Ok(edge_label_str) = edge_label.to_str() {
                    stack.push(ExportNode {
                        offset: edge_offset as usize,
                        prefix: format!(
                            "{}{}",
                            export_node.prefix, edge_label_str
                        ),
                    });
                }

                edge_remainder = remainder;
            }

            if length != 0 {
                self.exports.push(export_node.prefix)
            }
        }

        Ok((&[], ()))
    }

    /// Parser that parses the imports at the offsets defined within LC_DYLD_INFO and LC_DYLD_INFO_ONLY
    fn parse_imports(&mut self, data: &'a [u8]) -> IResult<&'a [u8], ()> {
        let mut remainder: &[u8] = data;
        let mut entry: u8;

        while !remainder.is_empty() {
            (remainder, entry) = u8(remainder)?;
            let opcode = entry & BIND_OPCODE_MASK;
            let _immediate = entry & BIND_IMMEDIATE_MASK;
            match opcode {
                BIND_OPCODE_SET_DYLIB_ORDINAL_ULEB
                | BIND_OPCODE_SET_SEGMENT_AND_OFFSET_ULEB
                | BIND_OPCODE_ADD_ADDR_ULEB
                | BIND_OPCODE_DO_BIND_ADD_ADDR_ULEB => {
                    (remainder, _) = uleb128(remainder)?;
                }
                BIND_OPCODE_DO_BIND_ULEB_TIMES_SKIPPING_ULEB => {
                    (remainder, _) = uleb128(remainder)?;
                    (remainder, _) = uleb128(remainder)?;
                }
                BIND_OPCODE_SET_ADDEND_SLEB => {
                    (remainder, _) = sleb128(remainder)?;
                }

                BIND_OPCODE_SET_SYMBOL_TRAILING_FLAGS_IMM => {
                    let (import_remainder, strr) = map(
                        (take_till(|b| b == b'\x00'), tag("\x00")),
                        |(s, _)| BStr::new(s),
                    )
                    .parse(remainder)?;
                    remainder = import_remainder;
                    if let Ok(import) = strr.to_str() {
                        self.imports.push(import.to_string());
                    }
                }
                _ => {}
            }
        }

        Ok((remainder, ()))
    }

    /// Parser that parses the header for the chained fixups designated by LC_DYLD_CHAINED_FIXUPS.
    fn chained_fixup_header(
        &self,
    ) -> impl Parser<&'a [u8], Output = ChainedFixupsHeader, Error = NomError<'a>>
           + '_ {
        map(
            (
                u32(self.endianness), //  fixups_version
                u32(self.endianness), //  starts_offset
                u32(self.endianness), //  imports_offset
                u32(self.endianness), //  symbols_offset
                u32(self.endianness), //  imports_count
                u32(self.endianness), //  imports_format
                u32(self.endianness), //  symbols_format
            ),
            |(
                fixups_version,
                starts_offset,
                imports_offset,
                symbols_offset,
                imports_count,
                imports_format,
                symbols_format,
            )| {
                ChainedFixupsHeader {
                    _fixups_version: fixups_version,
                    _starts_offset: starts_offset,
                    imports_offset,
                    symbols_offset,
                    imports_count,
                    _imports_format: imports_format,
                    _symbols_format: symbols_format,
                }
            },
        )
    }

    /// Parser that parses the chained fixup imports designated by LC_DYLD_CHAINED_FIXUPS.
    fn parse_chained_fixups(
        &mut self,
        data: &'a [u8],
    ) -> IResult<&'a [u8], ()> {
        let (_, header) = self.chained_fixup_header().parse(data)?;

        if let Some(import_data) = data.get(header.imports_offset as usize..) {
            let mut remainder = import_data;
            let mut chained_import_value: u32;

            for _ in 0..header.imports_count {
                (remainder, chained_import_value) =
                    u32(self.endianness)(remainder)?;

                let _lib_ordinal = chained_import_value & 0xff;
                let _import_kind = (chained_import_value >> 8) & 0x1;
                let name_offset = chained_import_value >> 9;

                if let Some(name_buffer) = data.get(
                    header.symbols_offset.saturating_add(name_offset)
                        as usize..,
                ) {
                    let (_remainder, import_str) = map(
                        (take_till(|b| b == b'\x00'), tag("\x00")),
                        |(s, _)| s,
                    )
                    .parse(name_buffer)?;

                    if let Ok(import) = import_str.to_str() {
                        self.imports.push(import.to_string());
                    }
                }
            }
        }

        Ok((&[], ()))
    }

    /// Parser that parses a LC_ID_DYLINKER, LC_LOAD_DYLINKER or
    /// LC_DYLD_ENVIRONMENT  command.
    fn dylinker_command(
        &self,
    ) -> impl Parser<&'a [u8], Output = &'a [u8], Error = NomError<'a>> + '_
    {
        map(
            (
                u32(self.endianness),        // offset,
                take_till(|b| b == b'\x00'), // command
            ),
            |(_offset, command)| command,
        )
    }

    /// Parser that parses a LC_LINKER_OPTION command.
    fn linker_options_command(
        &self,
    ) -> impl Parser<&'a [u8], Output = Vec<&'a [u8]>, Error = NomError<'a>> + '_
    {
        length_count(
            u32(self.endianness), // count
            map((take_till(|b| b == b'\x00'), tag("\x00")), |(s, _)| s),
        )
    }

    /// Parser that parses a LC_UUID command.
    fn uuid_command(
        &self,
    ) -> impl Parser<&'a [u8], Output = &'a [u8], Error = NomError<'a>> + '_
    {
        map(take(16usize), |uuid| BStr::new(uuid).trim_end_with(|c| c == '\0'))
    }

    /// Parser that parses a LC_SOURCE_VERSION command.
    fn source_version_command(
        &self,
    ) -> impl Parser<&'a [u8], Output = u64, Error = NomError<'a>> + '_ {
        u64(self.endianness)
    }

    /// Parser that parses a LC_RPATH command.
    fn rpath_command(
        &self,
    ) -> impl Parser<&'a [u8], Output = &'a [u8], Error = NomError<'a>> + '_
    {
        map(
            (
                u32(self.endianness),
                take_till(|b| b == b'\x00'), // rpath
            ),
            |(_, rpath)| rpath,
        )
    }

    /// Parser that parses a LC_BUILD_VERSION command.
    fn build_version_command(
        &self,
    ) -> impl Parser<&'a [u8], Output = BuildVersionCommand, Error = NomError<'a>>
           + '_ {
        map(
            (
                u32(self.endianness), // platform,
                u32(self.endianness), // minos,
                u32(self.endianness), // sdk,
                length_count(
                    u32(self.endianness), // ntools,
                    map(
                        (
                            u32(self.endianness), // tool,
                            u32(self.endianness), // version,
                        ),
                        |(tool, version)| BuildToolObject { tool, version },
                    ),
                ),
            ),
            |(platform, minos, sdk, tools)| BuildVersionCommand {
                platform,
                minos,
                sdk,
                tools,
            },
        )
    }

    fn min_version_command(
        &self,
    ) -> impl Parser<&'a [u8], Output = MinVersion, Error = NomError<'a>> + '_
    {
        map(
            (
                u32(self.endianness), // version
                u32(self.endianness), // sdk,
            ),
            |(version, sdk)| MinVersion { device: 0, version, sdk },
        )
    }

    fn x86_thread_state(
        &self,
    ) -> impl Parser<&'a [u8], Output = u64, Error = NomError<'a>> + '_ {
        map(
            (
                u32(self.endianness), // eax
                u32(self.endianness), // ebx
                u32(self.endianness), // ecx
                u32(self.endianness), // edx
                u32(self.endianness), // edi
                u32(self.endianness), // esi
                u32(self.endianness), // ebp
                u32(self.endianness), // esp
                u32(self.endianness), // ss
                u32(self.endianness), // eflags
                u32(self.endianness), // eip
                u32(self.endianness), // cs
                u32(self.endianness), // ds
                u32(self.endianness), // es
                u32(self.endianness), // fs
                u32(self.endianness), // gs
            ),
            |reg| reg.10 as u64, // eip,
        )
    }

    fn x86_64_thread_state(
        &self,
    ) -> impl Parser<&'a [u8], Output = u64, Error = NomError<'a>> + '_ {
        map(
            (
                u64(self.endianness), // rax
                u64(self.endianness), // rbx
                u64(self.endianness), // rcx
                u64(self.endianness), // rdx
                u64(self.endianness), // rdi
                u64(self.endianness), // rsi
                u64(self.endianness), // rbp
                u64(self.endianness), // rsp
                u64(self.endianness), // r8
                u64(self.endianness), // r9
                u64(self.endianness), // r10
                u64(self.endianness), // r11
                u64(self.endianness), // r12
                u64(self.endianness), // r13
                u64(self.endianness), // r14
                u64(self.endianness), // r15
                u64(self.endianness), // rip
                u64(self.endianness), // rflags
                u64(self.endianness), // cs
                u64(self.endianness), // fs
                u64(self.endianness), // gs
            ),
            |reg| reg.16, // eip,
        )
    }

    fn arm_thread_state(
        &self,
    ) -> impl Parser<&'a [u8], Output = u64, Error = NomError<'a>> + '_ {
        map(
            (
                count(u32(self.endianness), 13), // r
                u32(self.endianness),            // sp
                u32(self.endianness),            // lr
                u32(self.endianness),            // pc
                u32(self.endianness),            // cpsr
            ),
            |(_, _, _, pc, _)| pc as u64,
        )
    }

    fn arm64_thread_state(
        &self,
    ) -> impl Parser<&'a [u8], Output = u64, Error = NomError<'a>> + '_ {
        map(
            (
                count(u64(self.endianness), 29), // r
                u64(self.endianness),            // fp
                u64(self.endianness),            // lr
                u64(self.endianness),            // sp
                u64(self.endianness),            // pc
                u32(self.endianness),            // cpsr
            ),
            |(_, _, _, _, pc, _)| pc,
        )
    }

    fn ppc_thread_state(
        &self,
    ) -> impl Parser<&'a [u8], Output = u64, Error = NomError<'a>> + '_ {
        map(
            (
                uint(self.endianness, true),            // srr0
                uint(self.endianness, true),            // srr1
                count(uint(self.endianness, true), 32), // r
                uint(self.endianness, true),            // cr
                uint(self.endianness, true),            // xer
                uint(self.endianness, true),            // lr
                uint(self.endianness, true),            // ctr
                uint(self.endianness, true),            // mq
                uint(self.endianness, true),            // vrsavead
            ),
            |(srr0, _, _, _, _, _, _, _, _)| srr0,
        )
    }

    fn ppc64_thread_state(
        &self,
    ) -> impl Parser<&'a [u8], Output = u64, Error = NomError<'a>> + '_ {
        map(
            (
                uint(self.endianness, false),            // srr0
                uint(self.endianness, false),            // srr1
                count(uint(self.endianness, false), 32), // r
                uint(self.endianness, true),             // cr
                uint(self.endianness, false),            // xer
                uint(self.endianness, false),            // lr
                uint(self.endianness, false),            // ctr
                uint(self.endianness, false),            // vrsave
            ),
            |(srr0, _, _, _, _, _, _, _)| srr0,
        )
    }

    fn sparc_thread_state(
        &self,
    ) -> impl Parser<&'a [u8], Output = u64, Error = NomError<'a>> + '_ {
        map(
            (
                u32(self.endianness),           // psr
                u32(self.endianness),           // pc
                u32(self.endianness),           // npc
                u32(self.endianness),           // y
                count(u32(self.endianness), 7), // g
                count(u32(self.endianness), 7), // o
            ),
            |(_, pc, _, _, _, _)| pc as u64,
        )
    }

    fn m68k_thread_state(
        &self,
    ) -> impl Parser<&'a [u8], Output = u64, Error = NomError<'a>> + '_ {
        map(
            (
                count(u32(self.endianness), 8), // dreg
                count(u32(self.endianness), 8), // areg
                u16(self.endianness),           // pad
                u16(self.endianness),           // sr
                u32(self.endianness),           // pc
            ),
            |(_, _, _, _, pc)| pc as u64,
        )
    }

    fn m88k_thread_state(
        &self,
    ) -> impl Parser<&'a [u8], Output = u64, Error = NomError<'a>> + '_ {
        map(
            (
                count(u32(self.endianness), 31), // r
                u32(self.endianness),            // xip
                u32(self.endianness),            // xip_in_bd
                u32(self.endianness),            // nip
            ),
            |(_, xip, _, _)| xip as u64,
        )
    }
}

struct FatArch {
    cputype: u32,
    cpusubtype: u32,
    offset: u64,
    size: u64,
    align: u32,
    reserved: u32,
}

struct MachOHeader {
    magic: u32,
    cputype: u32,
    cpusubtype: u32,
    filetype: u32,
    ncmds: u32,
    sizeofcmds: u32,
    flags: u32,
    reserved: Option<u32>, // Only set in 64-bits binary.
}

struct Segment<'a> {
    segname: &'a [u8],
    vmaddr: u64,
    vmsize: u64,
    fileoff: u64,
    filesize: u64,
    maxprot: u32,
    initprot: u32,
    nsects: u32,
    flags: u32,
    sections: Vec<Section<'a>>,
}

struct Section<'a> {
    sectname: &'a [u8],
    segname: &'a [u8],
    addr: u64,
    size: u64,
    offset: u32,
    align: u32,
    reloff: u32,
    nreloc: u32,
    flags: u32,
    reserved1: u32,
    reserved2: u32,
    reserved3: Option<u32>, // Only set in 64-bits binaries
}

struct Dylib<'a> {
    name: &'a [u8],
    timestamp: u32,
    current_version: u32,
    compatibility_version: u32,
}

#[derive(Default)]
struct Certificate {
    issuer: String,
    subject: String,
    is_self_signed: bool,
}

struct CSBlob {
    magic: u32,
    length: u32,
}

struct CSBlobIndex {
    _blobtype: u32,
    offset: u32,
    blob: Option<CSBlob>,
}

struct CSSuperBlob {
    _magic: u32,
    _length: u32,
    count: u32,
    index: Vec<CSBlobIndex>,
}

struct LinkedItData {
    dataoff: u32,
    datasize: u32,
}

struct Symtab<'a> {
    symoff: u32,
    nsyms: u32,
    stroff: u32,
    strsize: u32,
    entries: Vec<SymbolTableEntry<'a>>,
}

struct Nlist {
    n_strx: u32,
    n_type: u8,
    _n_sect: u8,
    _n_desc: u16,
    _n_value: u64,
}

struct SymbolTableEntry<'a> {
    tags: u8,
    value: &'a [u8],
}

struct Dysymtab {
    ilocalsym: u32,
    nlocalsym: u32,
    iextdefsym: u32,
    nextdefsym: u32,
    tocoff: u32,
    ntoc: u32,
    modtaboff: u32,
    nmodtab: u32,
    extrefsymoff: u32,
    nextrefsyms: u32,
    indirectsymoff: u32,
    nindirectsyms: u32,
    extreloff: u32,
    nextrel: u32,
    locreloff: u32,
    nlocrel: u32,
}

struct DyldInfo {
    rebase_off: u32,
    rebase_size: u32,
    bind_off: u32,
    bind_size: u32,
    weak_bind_off: u32,
    weak_bind_size: u32,
    lazy_bind_off: u32,
    lazy_bind_size: u32,
    export_off: u32,
    export_size: u32,
}

struct DyldExportTrie {
    data_off: u32,
    data_size: u32,
}

struct DyldChainFixups {
    data_off: u32,
    data_size: u32,
}

struct ChainedFixupsHeader {
    _fixups_version: u32,
    _starts_offset: u32,
    imports_offset: u32,
    symbols_offset: u32,
    imports_count: u32,
    _imports_format: u32,
    _symbols_format: u32,
}

struct BuildVersionCommand {
    platform: u32,
    minos: u32,
    sdk: u32,
    tools: Vec<BuildToolObject>,
}

struct BuildToolObject {
    tool: u32,
    version: u32,
}

struct MinVersion {
    device: u32,
    version: u32,
    sdk: u32,
}

struct ExportNode {
    offset: usize,
    prefix: String,
}

/// Parser that reads a 32-bits or 64-bits
fn uint(
    endianness: Endianness,
    _32bits: bool,
) -> impl FnMut(&[u8]) -> IResult<&[u8], u64> {
    move |input: &[u8]| {
        if _32bits {
            let (remainder, i) = u32(endianness)(input)?;
            Ok((remainder, i as u64))
        } else {
            u64(endianness)(input)
        }
    }
}

/// Parser that reads [ULEB128][1].
///
/// Notice however that this function returns a `u64`, so it's able to parse
/// numbers up to 2^64-1. When parsing larger numbers it fails, even if they
/// are valid ULEB128.
///
/// [1]: https://en.wikipedia.org/wiki/LEB128
fn uleb128(input: &[u8]) -> IResult<&[u8], u64> {
    let mut val: u64 = 0;
    let mut shift: u32 = 0;

    let mut data = input;
    let mut byte: u8;

    loop {
        // Read one byte of data.
        (data, byte) = u8(data)?;

        // Use all the bits, except the most significant one.
        let b = (byte & 0x7f) as u64;

        val |= b
            .checked_shl(shift)
            .ok_or(Err::Error(NomError::new(input, ErrorKind::TooLarge)))?;

        // Break if the most significant bit is zero.
        if byte & 0x80 == 0 {
            break;
        }

        shift += 7;
    }

    Ok((data, val))
}

/// Parser that reads [SLEB128][1].
///
/// Notice however that this function returns an `i64`, so it's able to parse
/// numbers from -2^63 to 2^63-1. When parsing numbers out of that range it
/// fails, even if they are valid ULEB128.
///
/// [1]: https://en.wikipedia.org/wiki/LEB128
fn sleb128(input: &[u8]) -> IResult<&[u8], i64> {
    let mut val: i64 = 0;
    let mut shift: u32 = 0;

    let mut data = input;
    let mut byte: u8;

    loop {
        (data, byte) = u8(data)?;

        // Use all the bits, except the most significant one.
        let b = (byte & 0x7f) as i64;

        val |= b
            .checked_shl(shift)
            .ok_or(Err::Error(NomError::new(input, ErrorKind::TooLarge)))?;

        shift += 7;

        // Break if the most significant bit is zero.
        if byte & 0x80 == 0 {
            break;
        }
    }

    if shift < i64::BITS && (byte & 0x40) != 0 {
        val |= !0 << shift;
    }

    Ok((data, val))
}

/// Convert a decimal number representation to a version string representation.
fn convert_to_version_string(decimal_number: u32) -> String {
    let major = decimal_number >> 16;
    let minor = (decimal_number >> 8) & 0xFF;
    let patch = decimal_number & 0xFF;
    format!("{major}.{minor}.{patch}")
}

/// Convert a decimal number representation to a build version string representation.
fn convert_to_build_tool_version(decimal_number: u32) -> String {
    let a = decimal_number >> 16;
    let b = (decimal_number >> 8) & 0xff;
    format!("{a}.{b}")
}

/// Convert a decimal number representation to a source version string
/// representation.
fn convert_to_source_version_string(decimal_number: u64) -> String {
    let mask = 0x3f;
    let a = decimal_number >> 40;
    let b = (decimal_number >> 30) & mask;
    let c = (decimal_number >> 20) & mask;
    let d = (decimal_number >> 10) & mask;
    let e = decimal_number & mask;
    format!("{a}.{b}.{c}.{d}.{e}")
}

/// Parses CMS certificates from a BER-encoded blob that are embedded in the
/// Mach-O binary.
fn parse_certificates(ber_blob: &[u8]) -> BerResult<'_, Vec<Certificate>> {
    parse_ber_sequence_defined_g(|ber_blob: &[u8], _| {
        let (remainder, _content_type) = parse_ber_oid(ber_blob)?;

        parse_ber_tagged_explicit_g(0, |content, _| {
            parse_ber_sequence_defined_g(|content: &[u8], _| {
                let (remainder, _cms_version) = parse_ber_integer(content)?;

                let (remainder, _digest_algorithms) =
                    parse_digest_algorithms(remainder)?;

                let (remainder, _content_info) =
                    parse_content_info(remainder)?;

                let (remainder, certificates) = OptTaggedParser::from(0)
                    .parse_ber(
                        remainder,
                        |_, raw_certs| -> ParseResult<'_, Vec<_>> {
                            Ok(SignedData::parse_certificates(raw_certs))
                        },
                    )
                    .map_err(|_| BerValueError)?;

                let certificates: Vec<Certificate> = certificates
                    .iter()
                    .flatten()
                    .map(|c| Certificate {
                        issuer: c.x509.issuer.to_string(),
                        subject: c.x509.subject.to_string(),
                        is_self_signed: c.x509.issuer == c.x509.subject,
                    })
                    .collect();

                Ok((remainder, certificates))
            })(content)
        })(remainder)
    })(ber_blob)
}

/// Parses a BER-encoded sequence of AlgorithmIdentifiers.
fn parse_digest_algorithms(
    input: &[u8],
) -> BerResult<'_, Vec<AlgorithmIdentifier<'_>>> {
    Ok(parse_ber_set_of_v(AlgorithmIdentifier::from_ber)(input)
        .map_err(|_| BerValueError)?)
}

/// Parses a BER-encoded sequence of ContentInfo objects.
fn parse_content_info(input: &[u8]) -> BerResult<'_> {
    parse_ber_sequence(input)
}

impl From<MachO<'_>> for protos::macho::Macho {
    fn from(macho: MachO<'_>) -> Self {
        let mut result = protos::macho::Macho::new();
        // If the Mach-O file is a single-architecture binary, fill the fields
        // at the top level of `protos::macho::Macho` structure. If it is a
        // multi-architecture binary (FAT binary) then fill the `fat_arch`
        // and `file` arrays.
        if macho.files.len() == 1 {
            let m = macho.files.first().unwrap();
            result.set_magic(m.header.magic);
            result.set_ncmds(m.header.ncmds);
            result.set_cputype(m.header.cputype);
            result.set_cpusubtype(m.header.cpusubtype);
            result.set_filetype(m.header.filetype);
            result.set_flags(m.header.flags);
            result.set_sizeofcmds(m.header.sizeofcmds);
            result.reserved = m.header.reserved;
            result.entry_point = m.entry_point_offset;
            result.stack_size = m.stack_size;
            m.source_version.clone_into(&mut result.source_version);
            result.dynamic_linker = m.dynamic_linker.map(|dl| dl.into());

            if let Some(symtab) = &m.symtab {
                result.symtab = MessageField::some(symtab.into());
            }

            if let Some(dysymtab) = &m.dysymtab {
                result.dysymtab = MessageField::some(dysymtab.into());
            }

            if let Some(cs_data) = &m.code_signature_data {
                result.code_signature_data =
                    MessageField::some(cs_data.into());
            }

            if let Some(dyld_info) = &m.dyld_info {
                result.dyld_info = MessageField::some(dyld_info.into());
            };

            if let Some(uuid) = &m.uuid {
                let mut uuid_str = String::new();

                for (idx, c) in uuid.iter().enumerate() {
                    match idx {
                        3 | 5 | 7 | 9 => {
                            uuid_str.push_str(format!("{c:02X}").as_str());
                            uuid_str.push('-');
                        }
                        _ => {
                            uuid_str.push_str(format!("{c:02X}").as_str());
                        }
                    }
                }

                result.uuid = Some(uuid_str.clone());
            }

            if let Some(bv) = &m.build_version {
                result.build_version = MessageField::some(bv.into());
            }

            if let Some(mv) = &m.min_version {
                result.min_version = MessageField::some(mv.into());
            }

            result.segments.extend(m.segments.iter().map(|seg| seg.into()));
            result.dylibs.extend(m.dylibs.iter().map(|dylib| dylib.into()));
            result
                .rpaths
                .extend(m.rpaths.iter().map(|rpath: &&[u8]| rpath.to_vec()));
            result.entitlements.extend(m.entitlements.clone());
            result.exports.extend(m.exports.clone());
            result.imports.extend(m.imports.clone());

            // If the exports are empty, iterate the symbol table entries to
            // check like dyld_info does:
            // https://github.com/apple-oss-distributions/dyld/blob/main/other-tools/dyld_info.cpp#L560-L617
            if m.dyld_export_trie.is_none() && m.dyld_info.is_none() {
                if let Some(symtab) = &m.symtab {
                    result.exports.extend(symtab.entries.iter().filter_map(
                        |e| {
                            let t = e.tags & N_TYPE;

                            if (e.tags & N_EXT != 0)
                                && ((t == N_SECT)
                                    || (t == N_ABS)
                                    || (t == N_INDR))
                                && ((e.tags & N_STAB) == 0)
                            {
                                Some(BStr::new(e.value).to_string())
                            } else {
                                None
                            }
                        },
                    ))
                }
            }

            // If the imports are empty, iterate the symbol table entries to
            // check for undefined symbols like dyld_info does:
            // https://github.com/apple-oss-distributions/dyld/blob/main/other-tools/dyld_info.cpp#L372-L398
            if m.dyld_chain_fixups.is_none() && m.dyld_info.is_none() {
                if let Some(symtab) = &m.symtab {
                    result.imports.extend(symtab.entries.iter().filter_map(
                        |e| {
                            let t = e.tags & N_TYPE;

                            if (t == N_UNDF) && (e.tags & N_STAB == 0) {
                                Some(BStr::new(e.value).to_string())
                            } else {
                                None
                            }
                        },
                    ))
                }
            }

            result
                .certificates
                .extend(m.certificates.iter().map(|cert| cert.into()));

            result
                .set_number_of_segments(m.segments.len().try_into().unwrap());

            result
                .linker_options
                .extend(m.linker_options.iter().map(|lo| lo.to_vec()));
        } else {
            result.fat_magic = macho.fat_magic;
            result.set_nfat_arch(macho.archs.len().try_into().unwrap());
            result.fat_arch.extend(macho.archs.iter().map(|arch| arch.into()));
            result.file.extend(macho.files.iter().map(|file| file.into()));
        }
        result
    }
}

impl From<&MachOFile<'_>> for protos::macho::File {
    fn from(macho: &MachOFile<'_>) -> Self {
        let mut result = protos::macho::File::new();
        result.set_magic(macho.header.magic);
        result.set_ncmds(macho.header.ncmds);
        result.set_cputype(macho.header.cputype);
        result.set_cpusubtype(macho.header.cpusubtype);
        result.set_filetype(macho.header.filetype);
        result.set_flags(macho.header.flags);
        result.set_sizeofcmds(macho.header.sizeofcmds);
        result.reserved = macho.header.reserved;
        result.entry_point = macho.entry_point_offset;
        result.stack_size = macho.stack_size;
        macho.source_version.clone_into(&mut result.source_version);
        result.dynamic_linker = macho.dynamic_linker.map(|dl| dl.into());

        if let Some(symtab) = &macho.symtab {
            result.symtab = MessageField::some(symtab.into());
        }

        if let Some(dysymtab) = &macho.dysymtab {
            result.dysymtab = MessageField::some(dysymtab.into());
        }

        if let Some(cs_data) = &macho.code_signature_data {
            result.code_signature_data = MessageField::some(cs_data.into());
        }

        if let Some(dyld_info) = &macho.dyld_info {
            result.dyld_info = MessageField::some(dyld_info.into());
        };

        if let Some(uuid) = &macho.uuid {
            let mut uuid_str = String::new();

            for (idx, c) in uuid.iter().enumerate() {
                match idx {
                    3 | 5 | 7 | 9 => {
                        uuid_str.push_str(format!("{c:02X}").as_str());
                        uuid_str.push('-');
                    }
                    _ => {
                        uuid_str.push_str(format!("{c:02X}").as_str());
                    }
                }
            }

            result.uuid = Some(uuid_str.clone());
        }

        if let Some(bv) = &macho.build_version {
            result.build_version = MessageField::some(bv.into());
        }

        if let Some(mv) = &macho.min_version {
            result.min_version = MessageField::some(mv.into());
        }

        result.segments.extend(macho.segments.iter().map(|seg| seg.into()));
        result.dylibs.extend(macho.dylibs.iter().map(|dylib| dylib.into()));
        result.rpaths.extend(macho.rpaths.iter().map(|rpath| rpath.to_vec()));
        result.entitlements.extend(macho.entitlements.clone());
        result.exports.extend(macho.exports.clone());
        result.imports.extend(macho.imports.clone());

        // If the exports are empty, iterate the symbol table entries to check
        // like dyld_info does:
        // https://github.com/apple-oss-distributions/dyld/blob/main/other-tools/dyld_info.cpp#L560-L617
        if macho.dyld_export_trie.is_none() && macho.dyld_info.is_none() {
            if let Some(symtab) = &macho.symtab {
                result.exports.extend(symtab.entries.iter().filter_map(|e| {
                    let t = e.tags & N_TYPE;

                    if (e.tags & N_EXT != 0)
                        && ((t == N_SECT) || (t == N_ABS) || (t == N_INDR))
                        && ((e.tags & N_STAB) == 0)
                    {
                        Some(BStr::new(e.value).to_string())
                    } else {
                        None
                    }
                }))
            }
        }

        // If the imports are empty, iterate the symbol table entries to
        // check for undefined symbols like dyld_info does:
        // https://github.com/apple-oss-distributions/dyld/blob/main/other-tools/dyld_info.cpp#L372-L398
        if macho.dyld_chain_fixups.is_none() && macho.dyld_info.is_none() {
            if let Some(symtab) = &macho.symtab {
                result.imports.extend(symtab.entries.iter().filter_map(|e| {
                    let t = e.tags & N_TYPE;

                    if (t == N_UNDF) && (e.tags & N_STAB == 0) {
                        Some(BStr::new(e.value).to_string())
                    } else {
                        None
                    }
                }))
            }
        }
        result
            .certificates
            .extend(macho.certificates.iter().map(|cert| cert.into()));

        result
            .linker_options
            .extend(macho.linker_options.iter().map(|lo| lo.to_vec()));

        result
            .set_number_of_segments(result.segments.len().try_into().unwrap());

        result
    }
}

impl From<&FatArch> for protos::macho::FatArch {
    fn from(arch: &FatArch) -> Self {
        let mut result = protos::macho::FatArch::new();
        result.set_cputype(arch.cputype);
        result.set_cpusubtype(arch.cpusubtype);
        result.set_offset(arch.offset);
        result.set_size(arch.size);
        result.set_align(arch.align);
        result.set_reserved(arch.reserved);
        result
    }
}

impl From<&Segment<'_>> for protos::macho::Segment {
    fn from(seg: &Segment<'_>) -> Self {
        let mut result = protos::macho::Segment::new();
        result.set_segname(seg.segname.into());
        result.set_vmaddr(seg.vmaddr);
        result.set_vmsize(seg.vmsize);
        result.set_fileoff(seg.fileoff);
        result.set_filesize(seg.filesize);
        result.set_maxprot(seg.maxprot);
        result.set_initprot(seg.initprot);
        result.set_nsects(seg.nsects);
        result.set_flags(seg.flags);
        result.sections.extend(seg.sections.iter().map(|sec| sec.into()));
        result
    }
}

impl From<&Section<'_>> for protos::macho::Section {
    fn from(sec: &Section<'_>) -> Self {
        let mut result = protos::macho::Section::new();
        result.set_segname(sec.segname.into());
        result.set_sectname(sec.sectname.into());
        result.set_addr(sec.addr);
        result.set_size(sec.size);
        result.set_offset(sec.offset);
        result.set_align(sec.align);
        result.set_reloff(sec.reloff);
        result.set_nreloc(sec.nreloc);
        result.set_flags(sec.flags);
        result.set_reserved1(sec.reserved1);
        result.set_reserved2(sec.reserved2);
        result.reserved3 = sec.reserved3;
        result
    }
}

impl From<&Dylib<'_>> for protos::macho::Dylib {
    fn from(dylib: &Dylib<'_>) -> Self {
        let mut result = protos::macho::Dylib::new();
        result.set_name(dylib.name.into());
        result.set_timestamp(dylib.timestamp);
        result.set_compatibility_version(convert_to_version_string(
            dylib.compatibility_version,
        ));
        result.set_current_version(convert_to_version_string(
            dylib.current_version,
        ));
        result
    }
}

impl From<&Symtab<'_>> for protos::macho::Symtab {
    fn from(symtab: &Symtab<'_>) -> Self {
        let mut result = protos::macho::Symtab::new();
        result.set_symoff(symtab.symoff);
        result.set_nsyms(symtab.nsyms);
        result.set_stroff(symtab.stroff);
        result.set_strsize(symtab.strsize);
        // populate the entries
        result
            .entries
            .extend(symtab.entries.iter().map(|entry| entry.value.to_vec()));
        result
    }
}

impl From<&Dysymtab> for protos::macho::Dysymtab {
    fn from(dysymtab: &Dysymtab) -> Self {
        let mut result = protos::macho::Dysymtab::new();
        result.set_ilocalsym(dysymtab.ilocalsym);
        result.set_nlocalsym(dysymtab.nlocalsym);
        result.set_iextdefsym(dysymtab.iextdefsym);
        result.set_nextdefsym(dysymtab.nextdefsym);
        result.set_tocoff(dysymtab.tocoff);
        result.set_ntoc(dysymtab.ntoc);
        result.set_modtaboff(dysymtab.modtaboff);
        result.set_nmodtab(dysymtab.nmodtab);
        result.set_extrefsymoff(dysymtab.extrefsymoff);
        result.set_nextrefsyms(dysymtab.nextrefsyms);
        result.set_indirectsymoff(dysymtab.indirectsymoff);
        result.set_nindirectsyms(dysymtab.nindirectsyms);
        result.set_extreloff(dysymtab.extreloff);
        result.set_nextrel(dysymtab.nextrel);
        result.set_locreloff(dysymtab.locreloff);
        result.set_nlocrel(dysymtab.nlocrel);
        result
    }
}

impl From<&LinkedItData> for protos::macho::LinkedItData {
    fn from(lid: &LinkedItData) -> Self {
        let mut result = protos::macho::LinkedItData::new();
        result.set_dataoff(lid.dataoff);
        result.set_datasize(lid.datasize);
        result
    }
}

impl From<&Certificate> for protos::macho::Certificate {
    fn from(cert: &Certificate) -> Self {
        let mut result = protos::macho::Certificate::new();
        result.set_issuer(cert.issuer.clone());
        result.set_subject(cert.subject.clone());
        result.set_is_self_signed(cert.is_self_signed);
        result
    }
}

impl From<&DyldInfo> for protos::macho::DyldInfo {
    fn from(dyld_info: &DyldInfo) -> Self {
        let mut result = protos::macho::DyldInfo::new();
        result.set_rebase_off(dyld_info.rebase_off);
        result.set_rebase_size(dyld_info.rebase_size);
        result.set_bind_off(dyld_info.bind_off);
        result.set_bind_size(dyld_info.bind_size);
        result.set_weak_bind_off(dyld_info.weak_bind_off);
        result.set_weak_bind_size(dyld_info.weak_bind_size);
        result.set_lazy_bind_off(dyld_info.lazy_bind_off);
        result.set_lazy_bind_size(dyld_info.lazy_bind_size);
        result.set_export_off(dyld_info.export_off);
        result.set_export_size(dyld_info.export_size);
        result
    }
}

impl From<&BuildVersionCommand> for protos::macho::BuildVersion {
    fn from(bv: &BuildVersionCommand) -> Self {
        let mut result = protos::macho::BuildVersion::new();
        result.set_platform(bv.platform);
        result.set_ntools(bv.tools.len() as u32);
        result.set_minos(convert_to_version_string(bv.minos));
        result.set_sdk(convert_to_version_string(bv.sdk));
        result.tools.extend(bv.tools.iter().map(|tool| tool.into()));
        result
    }
}

impl From<&BuildToolObject> for protos::macho::BuildTool {
    fn from(bt: &BuildToolObject) -> Self {
        let mut result = protos::macho::BuildTool::new();
        result.set_tool(bt.tool);
        result.set_version(convert_to_build_tool_version(bt.version));
        result
    }
}

impl From<&MinVersion> for protos::macho::MinVersion {
    fn from(mv: &MinVersion) -> Self {
        let mut result = protos::macho::MinVersion::new();

        result.set_device(
            protobuf::EnumOrUnknown::<protos::macho::DeviceType>::from_i32(
                mv.device as i32,
            )
            .unwrap(),
        );
        result.set_version(convert_to_version_string(mv.version));
        result.set_sdk(convert_to_version_string(mv.sdk));
        result
    }
}

#[test]
fn test_uleb_parsing() {
    let (_, n) = uleb128(&[0b1000_0001, 0b000_0001]).unwrap();
    assert_eq!(129, n);

    let (_, n) = uleb128(&[0b1000_0000, 0b0000_0001]).unwrap();
    assert_eq!(128, n);

    let (_, n) = uleb128(&[0b111_1111]).unwrap();
    assert_eq!(127, n);

    let (_, n) = uleb128(&[0b111_1110]).unwrap();
    assert_eq!(126, n);

    let (_, n) = uleb128(&[0b000_0000]).unwrap();
    assert_eq!(0, n);

    let (_, n) = uleb128(&[0b1010_0000, 0b0000_0001]).unwrap();
    assert_eq!(160, n);

    let (_, n) = uleb128(&[0b1001_0110, 0b0000_0101]).unwrap();
    assert_eq!(662, n);

    let (_, n) = uleb128(&[0b1110_0101, 0b1000_1110, 0b0010_0110]).unwrap();
    assert_eq!(624485, n);

    let (_, n) = uleb128(&[0x80, 0x80, 0x80, 0x00]).unwrap();
    assert_eq!(0, n);

    let (_, n) =
        uleb128(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00]).unwrap();
    assert_eq!(0, n);

    let (_, n) =
        uleb128(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]).unwrap();
    assert_eq!(72057594037927935, n);

    assert!(uleb128(&[
        0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00,
    ])
    .is_err());
}

#[test]
fn test_sleb_parsing() {
    let sleb_128_in = vec![0b1100_0111, 0b1001_1111, 0b111_1111];
    let (_remainder, result) = sleb128(&sleb_128_in).unwrap();
    assert_eq!(-12345, result);

    let sleb_128_in = vec![0b1001_1100, 0b111_1111];
    let (_remainder, result) = sleb128(&sleb_128_in).unwrap();
    assert_eq!(-100, result);

    let sleb_128_in = vec![0b1111_1111, 0b0];
    let (_remainder, result) = sleb128(&sleb_128_in).unwrap();
    assert_eq!(127, result);

    let sleb_128_in = vec![0b111_1111];
    let (_remainder, result) = sleb128(&sleb_128_in).unwrap();
    assert_eq!(-1, result);

    let sleb_128_in = vec![0b1111_1110, 0b0];
    let (_remainder, result) = sleb128(&sleb_128_in).unwrap();
    assert_eq!(126, result);

    let sleb_128_in = vec![0b000_0000];
    let (_remainder, result) = sleb128(&sleb_128_in).unwrap();
    assert_eq!(0, result);

    assert!(sleb128(&[
        0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00,
    ])
    .is_err());
}
