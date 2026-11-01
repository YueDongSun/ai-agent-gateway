export function inspect_windows_image(bytes) {
  const invalid = () => { throw new Error('desktop_native_invalid_pe_image'); };
  if (!Buffer.isBuffer(bytes) || bytes.length < 64 || bytes.readUInt16LE(0) !== 0x5a4d) invalid();
  const pe_offset = bytes.readUInt32LE(0x3c);
  if (pe_offset < 64 || pe_offset + 24 > bytes.length || bytes.readUInt32LE(pe_offset) !== 0x4550) invalid();
  const optional_offset = pe_offset + 24;
  const optional_size = bytes.readUInt16LE(pe_offset + 20);
  if (optional_size < 70 || optional_offset + optional_size > bytes.length) invalid();
  const magic = bytes.readUInt16LE(optional_offset);
  if (magic !== 0x10b && magic !== 0x20b) invalid();
  const subsystem = bytes.readUInt16LE(optional_offset + 68);
  return { machine: bytes.readUInt16LE(pe_offset + 4), subsystem, windows_gui: subsystem === 2 };
}
