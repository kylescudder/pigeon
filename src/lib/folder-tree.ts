import type { MailFolder } from "./native";

export type FolderNode = {
  /** The folder's full path, unique within an account. */
  path: string;
  label: string;
  /** Absent for a parent the server lists as unselectable. */
  folder?: MailFolder;
  children: FolderNode[];
};

// Builds the hierarchy from each folder's server-reported separator. Folders
// keep the order they arrive in, so the inbox stays first.
export function buildFolderTree(folders: MailFolder[]): FolderNode[] {
  const roots: FolderNode[] = [];
  const nodes = new Map<string, FolderNode>();
  for (const folder of folders) {
    const delimiter = folder.delimiter;
    const parts =
      delimiter && delimiter.length === 1
        ? folder.name.split(delimiter)
        : [folder.name];
    let siblings = roots;
    let path = "";
    parts.forEach((part, index) => {
      path = index === 0 ? part : `${path}${delimiter}${part}`;
      let node = nodes.get(path);
      if (!node) {
        node = { path, label: part, children: [] };
        nodes.set(path, node);
        siblings.push(node);
      }
      if (index === parts.length - 1) node.folder = folder;
      siblings = node.children;
    });
  }
  return roots;
}

export function subtreeUnread(node: FolderNode): number {
  return node.children.reduce(
    (total, child) => total + subtreeUnread(child),
    node.folder?.unread ?? 0,
  );
}

/** Paths of every ancestor of the folder with this id. */
export function ancestorPaths(nodes: FolderNode[], id: string | undefined) {
  const found: string[] = [];
  const walk = (list: FolderNode[], trail: string[]): boolean =>
    list.some((node) =>
      node.folder?.id === id
        ? (found.push(...trail), true)
        : walk(node.children, [...trail, node.path]),
    );
  if (id) walk(nodes, []);
  return found;
}
