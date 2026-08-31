// Mock Tauri API for bundling
export const invoke = async (cmd, args) => {
  throw new Error('Not in Tauri context');
};
export const getCurrentWindow = () => ({ close: () => {}, minimize: () => {} });
