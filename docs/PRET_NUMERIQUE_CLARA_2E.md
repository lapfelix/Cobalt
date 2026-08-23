# Clara 2E / N506 validation gate

The target reader for Prêt numérique is a Kobo Clara 2E, model N506. Its
measured read-only profile is now registered as `CLARA_2E_N506`; Clara BW N365
values and the existing `CLARA_BW_391` profile were not copied. Similar names
do not make the framebuffer, touch transform, rotation, firmware, or write
behavior interchangeable.

The physical reader reported:

```text
model=N506 firmware=4.38.23697 kernel=4.1.15 device-code=386
device-tree compatible=fsl,imx6sll-lpddr3-arm2,fsl,imx6sll
device-tree model=Freescale i.MX6SLL NTX Board
framebuffer=mxc_epdc_fb 1072x1448 virtual=1088x1536 stride=4352 map=6782976 rotation=3
touch=fts_ts /dev/input/event1 X=0..1448 Y=0..1072
```

Read-only doctor, the physical touch transform, the GC16 refresh, the
reversible pixel restore, and the whole-screen snapshot/restore now match the
observed reader. The N506 also uses the older 68-byte NTX `mxc_epdc_fb` update
ABI; the display layer has a separate implementation for it. `write_ready`
remains false until exit and restart evidence is reviewed.

For a repeat probe:

```text
cd Cobalt
cargo run -p kobo-cli -- devices
cargo run -p kobo-cli -- doctor --device <reader-ip>
```

`kobo doctor` is read-only. Record its device-tree compatible/model values,
framebuffer geometry and pixel fields, touch device/ranges, serial prefix,
firmware, kernel, and device code. Compare all fields against the
`DeviceProfile` validation in `crates/kobo-profile/src/lib.rs`.

The profile is covered by profile/layout tests and the touch and display
evidence is recorded from the physical probe. Keep `write_ready: false` until
exit and restart smoke evidence has been reviewed on that same reader. Then
run the remaining Cobalt porting smoke tests and install the package on the
Clara 2E before enabling ordinary writes.

The Prêt numérique app itself remains safe to develop without this profile:
its network/UI code can be checked in the host workspace, but no device
package or framebuffer write is treated as Clara 2E-ready by this checkout.
