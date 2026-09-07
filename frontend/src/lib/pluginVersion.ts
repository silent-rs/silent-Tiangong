type SemverIdentifier = number | string;

type ParsedSemver = {
  core: [number, number, number];
  prerelease: SemverIdentifier[];
};

const parseSemver = (value: string): ParsedSemver | null => {
  const match = value.trim().match(
    /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/,
  );
  if (!match) return null;

  const prerelease = match[4]
    ? match[4].split('.').map((identifier): SemverIdentifier =>
        /^\d+$/.test(identifier) ? Number(identifier) : identifier,
      )
    : [];

  return {
    core: [Number(match[1]), Number(match[2]), Number(match[3])],
    prerelease,
  };
};

const comparePrerelease = (left: SemverIdentifier[], right: SemverIdentifier[]): number => {
  if (left.length === 0 || right.length === 0) {
    return left.length === right.length ? 0 : left.length === 0 ? 1 : -1;
  }

  const length = Math.max(left.length, right.length);
  for (let index = 0; index < length; index += 1) {
    const leftIdentifier = left[index];
    const rightIdentifier = right[index];
    if (leftIdentifier === undefined || rightIdentifier === undefined) {
      return leftIdentifier === rightIdentifier ? 0 : leftIdentifier === undefined ? -1 : 1;
    }
    if (leftIdentifier === rightIdentifier) continue;
    if (typeof leftIdentifier === 'number' && typeof rightIdentifier === 'number') {
      return leftIdentifier > rightIdentifier ? 1 : -1;
    }
    if (typeof leftIdentifier === 'number') return -1;
    if (typeof rightIdentifier === 'number') return 1;
    return leftIdentifier.localeCompare(rightIdentifier) > 0 ? 1 : -1;
  }
  return 0;
};

/** 仅当线上语义版本严格高于当前版本时返回 true。无效版本按不可升级处理。 */
export const isRemotePluginVersionNewer = (current: string, remote: string): boolean => {
  const currentVersion = parseSemver(current);
  const remoteVersion = parseSemver(remote);
  if (!currentVersion || !remoteVersion) return false;

  for (let index = 0; index < currentVersion.core.length; index += 1) {
    if (remoteVersion.core[index] === currentVersion.core[index]) continue;
    return remoteVersion.core[index] > currentVersion.core[index];
  }

  return comparePrerelease(remoteVersion.prerelease, currentVersion.prerelease) > 0;
};
