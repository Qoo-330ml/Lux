import type { ChangelogRelease } from "../types";
import { changelogReleases07 } from "./v0-7";
import { changelogReleases06 } from "./v0-6";
import { changelogReleases05 } from "./v0-5";
import { changelogReleases04 } from "./v0-4";
import { changelogReleases03 } from "./v0-3";
import { changelogReleases02 } from "./v0-2";
import { changelogReleases01 } from "./v0-1";

/** 版本条目按发布顺序倒序排列，最新版本在最前。 */
export const changelogReleases: ChangelogRelease[] = [
  ...changelogReleases07,
  ...changelogReleases06,
  ...changelogReleases05,
  ...changelogReleases04,
  ...changelogReleases03,
  ...changelogReleases02,
  ...changelogReleases01,
];
