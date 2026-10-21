import type { Elements } from '../dom';
import type { AuthManager } from '../auth';
import type { RoomClient } from '../room';
import type { PwaControls } from '../pwa';
import type { AccountPreferences } from '../account-preferences';
import type { CommunityUI } from '../community-ui';
import type { AppNavigation } from '../app-navigation';
import type { MediaControls } from '../media-controls';
import type { SocialChat } from '../social-chat';
import type { RoomSettingsScreen } from './room-settings';
import type { RoomLayout } from './room-layout';
import { loadScreenPresentationMode, saveScreenPresentationMode } from '../screen-presentation';
import {
  SettingsSheet,
  group,
  markSaved,
  note,
  selectRow,
  type SheetSection,
} from '../settings-sheet';
import { el } from '../ui';

interface SettingsStore {
  readonly room: RoomClient | null;
}
interface SettingsServices {
  readonly auth: Pick<AuthManager, 'isLoggedIn' | 'userId'>;
  readonly pwa: Pick<PwaControls, 'mountAccount'>;
  readonly accountPreferences: Pick<AccountPreferences, 'mountPrivacy'>;
  readonly community: Pick<
    CommunityUI,
    'settingsRoomContext' | 'hasSelectedRoom' | 'sections' | 'roomSections' | 'closeRoomContext'
  >;
  readonly navigation: Pick<AppNavigation, 'back' | 'opened'>;
  readonly mediaControls: Pick<MediaControls, 'sections'>;
  readonly socialChat: Pick<SocialChat, 'sections'>;
  readonly roomSettings: Pick<RoomSettingsScreen, 'sections'>;
  readonly layout: RoomLayout;
  readonly diagnosticsButton: HTMLButtonElement;
  showKeyboardShortcuts: () => void;
}

/** Compose account and room settings with owned navigation and panel restoration. */
export function createAppSettings(
  elements: Elements,
  store: SettingsStore,
  services: SettingsServices,
) {
  const { settingsPanel, homeSettingsHost, sidebarTabs, sidebarElement, roomScreen, joinScreen } =
    elements;
  let settingsReturnTab: 'chat' | 'users' | 'lobby' = 'chat';
  let settingsRestoreChat = false;
  let settingsRestorePanel = false;
  /** Account rules and app alerts on the home page, where no chat is open. */
  const homeNotificationsSection: SheetSection = {
    id: 'notifications',
    segment: 'you',
    label: 'Notifications',
    available: () => services.auth.isLoggedIn,
    status: () => 'Rules and app alerts',
    screen: () => ({
      title: 'Notifications',
      body: (context) => {
        const body = document.createElement('div');
        body.className = 'sheet-screen';
        services.pwa.mountAccount(body, () => context.current());
        return body;
      },
    }),
  };
  const homePrivacySection: SheetSection = {
    id: 'privacy',
    segment: 'you',
    label: 'Privacy',
    available: () => services.auth.isLoggedIn,
    status: () => 'Private messages and ignored accounts',
    screen: () => {
      let stop: (() => void) | undefined;
      return {
        title: 'Privacy',
        body: (context) => {
          const body = el('div', undefined, 'sheet-screen');
          const account = services.auth.userId;
          stop = services.accountPreferences.mountPrivacy(
            body,
            () => context.current() && services.auth.userId === account,
          );
          return body;
        },
        leave: () => stop?.(),
      };
    },
  };
  const presentationSection: SheetSection = {
    id: 'screen-sharing',
    segment: 'you',
    label: 'Screen sharing',
    status: () => (loadScreenPresentationMode() === 'motion' ? 'Motion' : 'Details'),
    screen: () => ({
      title: 'Screen sharing',
      body: () => {
        const select = document.createElement('select');
        select.append(new Option('Details', 'detail'), new Option('Motion', 'motion'));
        select.value = loadScreenPresentationMode();
        select.addEventListener('change', () => {
          saveScreenPresentationMode(select.value === 'motion' ? 'motion' : 'detail');
          markSaved(select);
        });
        return group(null, [
          note(
            'Details favors readable slides and text. Motion favors moving video. Applies the next time you choose Share screen.',
          ),
          selectRow('Optimize for', select),
        ]);
      },
    }),
  };
  let stopSettingsNavigation: (() => void) | undefined;
  const settingsSheet = new SettingsSheet({
    roomContext: () => services.community.settingsRoomContext,
    onBack: () => services.navigation.back(),
    onNavigate: (state, action) => {
      if (!state) {
        stopSettingsNavigation?.();
        stopSettingsNavigation = undefined;
        return;
      }
      stopSettingsNavigation = services.navigation.opened(
        {
          group: 'settings',
          destination: {
            kind: 'settings',
            ...(!services.community.hasSelectedRoom && state.section && { section: state.section }),
            segment: state.segment,
          },
          ...(services.auth.userId && { accountId: services.auth.userId }),
          canLeave: state.canLeave,
          close: () => settingsSheet.close(),
          restore: state.restore,
        },
        action === 'pop',
      );
    },
    sections: () =>
      store.room
        ? [
            ...services.mediaControls.sections(),
            presentationSection,
            services.layout.section,
            ...services.socialChat.sections(),
            ...services.community.sections(),
            ...(!services.community.hasSelectedRoom ? services.roomSettings.sections() : []),
            ...services.community.roomSections(),
          ]
        : [
            presentationSection,
            homePrivacySection,
            homeNotificationsSection,
            ...services.community.sections(),
            ...services.community.roomSections(),
          ],
    footer: () =>
      store.room
        ? [
            { label: 'Keyboard shortcuts', activate: services.showKeyboardShortcuts },
            { label: 'Diagnostics', activate: () => services.diagnosticsButton.click() },
          ]
        : [],
    host: () => (store.room ? settingsPanel : homeSettingsHost),
    onOpen: () => {
      if (store.room) {
        const active = Array.from(sidebarTabs).find((tab) => tab.classList.contains('active'))
          ?.dataset['tab'];
        settingsReturnTab = active === 'users' || active === 'lobby' ? active : 'chat';
        services.layout.selectTab('settings');
        sidebarElement.classList.add('settings-open');
        roomScreen.classList.add('settings-open');
        settingsRestoreChat = services.layout.desktop && services.layout.preferences.chatCollapsed;
        if (settingsRestoreChat) services.layout.preferences.chatCollapsed = false;
        settingsRestorePanel =
          !services.layout.desktop && services.layout.preferences.mobilePanelCollapsed;
        if (settingsRestorePanel) services.layout.preferences.mobilePanelCollapsed = false;
        services.layout.apply();
      } else {
        homeSettingsHost.hidden = false;
        joinScreen.classList.add('settings-open');
      }
    },
    onClose: () => {
      sidebarElement.classList.remove('settings-open');
      roomScreen.classList.remove('settings-open');
      services.layout.selectTab(settingsReturnTab);
      if (settingsRestoreChat) services.layout.preferences.chatCollapsed = true;
      if (settingsRestorePanel) services.layout.preferences.mobilePanelCollapsed = true;
      settingsRestoreChat = settingsRestorePanel = false;
      services.layout.apply();
      homeSettingsHost.hidden = true;
      joinScreen.classList.remove('settings-open');
      services.community.closeRoomContext();
    },
  });
  return {
    sheet: settingsSheet,
    dispose() {
      settingsSheet.close({ force: true });
      stopSettingsNavigation?.();
      stopSettingsNavigation = undefined;
    },
  };
}
