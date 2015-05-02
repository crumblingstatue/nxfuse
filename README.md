# nxfuse
Expose NX data as a FUSE file system

Use cases:

- Maplestory jukebox
  
  ```
  nxfuse Data.nx mountpoint/ > /dev/null &
  mpv mountpoint/Sound/
  ```
  
  Isn't that neat?
