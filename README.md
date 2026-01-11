## Usage

1. Backup btrfs subvolume to other storage.
2. Create a live boot environment based on backup.

## Source layout

Requirement:

- Source subvolume has a .snapshots dir or subvolume for local snapshot.
    - If use `snapper`, `backup_btrfs` will create a snapshot subvolume by snapper at `.snapshots/[id]/snapshot`.
    - If not, `backup_btrfs` will create a snapshot subvolume at `.snapshots/backup_btrfs`.

With local snapshot, `backup_btrfs` can use `btrfs send -p local_snap_old local_snap_new | btrfs receive ...` to do
incremental backup.

Example(simple):

```
(btrfs top-level subvolume)
├── root_vol                ->  /
├── root_snapshot_vol       ->  /.snapshots
├── home_user_vol           ->  /home/user
├── home_user_snapshot_vol  ->  /home/user/.snapshots
├── other_vol               ->  /any/mount/point/other
└── other_snapshot_vol      ->  /any/mount/point/other/.snapshots
```

Example(full):

```
(esp)                  ->  /efi

(btrfs top-level subvolume)
├── @snapshots
│   ├── root_vol       ->  /.snapshots
│   └── home_user_vol  ->  /home/user/.snapshots
└── @
    ├── root_vol       ->  /
    ├── home_user_vol  ->  /home/user
    ├── var_vol        ->  /var
    ├── ...
    └── swap_vol       ->  /swap
```


## Target layout

Target filesystem or subvolume is only a simple subvolume.

If enable live boot environment, target filesystem has two subvolume `@` and `@snapshots`.

- `@` for live boot environment.
- `@snapshots` for backup target (i.e. the target subvolume when live boot environment not enabled).

Example(without live boot):

```
(target subvolume)
├── root_vol
├── home_user_vol
└── other_vol
```

Example(with live boot):

```
(esp)                  ->  /efi

(btrfs top-level subvolume)
├── @snapshots
│   ├── root_vol
│   └── home_user_vol
└── @
    ├── root_vol       ->  /
    └── home_user_vol  ->  /home/user
```

## Live boot environment

### Prepare

If with live boot environment, external storage must be prepared.
`backup_btrfs` provides a command to do those things:

1. Check mounted filesystems (path provided by arguments).
    1. target btrfs filesystem (top-level subvolume mounted).
    2. esp of target storage (optional).
2. Create subvolums under target btrfs filesystem.
3. Init loader (optional).
    1. Install systemd-boot to esp of target storage.
    2. Generate a loader entry for systemd-boot.

You can edit the loader manually.

### Post backup

After backup, subvolume under `@` is replace by a snapshot of new backup.
So, live boot environment will be always updated.

And some hook will run on live boot environment:

1. Copy kernel and initramfs from `/boot` to `/efi`.
2. Regenerate `fstab` if new subvolume was added.
3. Remove any snapper config to prevent snapper creating snapshots on live boot environment.
