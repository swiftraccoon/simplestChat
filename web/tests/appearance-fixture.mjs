import { loadTypeScript } from './source-loader.mjs';

/** The appearance controls use standard CSS/DOM APIs outside the text-only fixture. */
export async function loadAppearanceFixture({ Node, ui }) {
  Object.defineProperties(Node.prototype, {
    dataset: {
      configurable: true,
      get() {
        return (this._dataset ??= {});
      },
      set(value) {
        this._dataset = value;
      },
    },
    style: {
      configurable: true,
      get() {
        return (this._style ??= {
          setProperty(name, value) {
            this[name] = value;
          },
        });
      },
      set(value) {
        this._style = value;
      },
    },
    classList: {
      configurable: true,
      get() {
        return {
          add: (value) => {
            const classes = new Set((this.className ?? '').split(/\s+/).filter(Boolean));
            classes.add(value);
            this.className = [...classes].join(' ');
          },
        };
      },
    },
  });
  const colors = await loadTypeScript('src/avatar-colors.ts');
  return loadTypeScript('src/appearance.ts', {
    modules: { './ui': ui, './avatar-colors': colors, './appearance.css': {} },
  });
}
