// The app's pages, as the site's routes load them: in the browser only.
import { onSettled } from 'solid-js';
import { MachinesPage, pairFromUrl } from '../src/components/Devices';
import { HostedScreen } from '../src/components/HostedScreen';
import { favoritePath, navigate } from '../src/hosted';

export function Home() {
  // The app icon's "Favorite" shortcut opens `/?favorite`.
  onSettled(() => { if (new URLSearchParams(location.search).has('favorite')) navigate(favoritePath(), true); });
  return <MachinesPage />;
}

export const PluginsPage = () => <MachinesPage plugins />;

export function PairPage() {
  onSettled(() => pairFromUrl());
  return <MachinesPage />;
}

export const ScreenPage = HostedScreen;
