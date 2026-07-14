import type { RepoStatus } from "../types/ipc";
import { ContextSlider } from "./ContextSlider";
import { ThemeToggle } from "./ThemeToggle";

type HeaderBarProps = {
  status: RepoStatus;
};

/**
 * Top fixed-height bar: repo/branch on the left; the context slider and the
 * theme toggle on the right.
 */
export function HeaderBar({ status }: HeaderBarProps) {
  return (
    <header className="header-bar">
      <div className="header-bar__title">
        <span className="header-bar__repo">{status.repoName}</span>
        <span className="header-bar__sep">—</span>
        <span className="header-bar__branch">branch: {status.branch}</span>
      </div>
      <div className="header-bar__slot">
        <ContextSlider />
        <ThemeToggle />
      </div>
    </header>
  );
}
