import './style.css';
import './community.css';
import './participant-hovercard.css';
import './notification-preferences.css';
import './discovery.css';
import { queryElements } from './dom';
import { startApp } from './app';

declare const __APP_REVISION__: string;

startApp(queryElements(document), __APP_REVISION__);
