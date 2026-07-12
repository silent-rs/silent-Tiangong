import { resolveAssetUrl } from "./utils";
import type { MessageItem } from "./types";

export function ContentMedia({ message }: { message: MessageItem }) {
  const content = Array.isArray(message.content) ? message.content : [];
  const mediaBlocks = content.flatMap((block) => {
    if (block.type === "media") {
      return [{
        kind: block.kind,
        url: block.url,
        title: block.title,
        mime_type: block.mime_type,
      }];
    }
    if (block.type === "asset_reference" || block.type === "image") {
      return [{
        kind: block.asset.kind,
        url: block.asset.local_path,
        title: block.asset.original_name,
        mime_type: block.asset.mime_type,
      }];
    }
    return [];
  });
  const legacyMedia = message.media || [];
  const allMedia = [
    ...mediaBlocks,
    ...legacyMedia,
  ];
  if (allMedia.length === 0) return null;
  return (
    <div className="space-y-2 my-2">
      {allMedia.map((asset, index) => {
        if (asset.url === "<legacy-inline-data-unavailable>") {
          return <div key={`${message.id}-media-${index}`} className="text-sm text-muted-foreground">旧附件无法安全恢复，请重新上传。</div>;
        }
        const src = resolveAssetUrl(asset.url);
        if (asset.kind === "image") {
          return <img key={`${message.id}-media-${index}`} src={src} alt={asset.title || "生成的图片"} className="max-w-full max-h-96 rounded-md cursor-pointer hover:opacity-90 transition-opacity" loading="lazy" />;
        }
        if (asset.kind === "video") {
          return <video key={`${message.id}-media-${index}`} src={src} controls className="max-w-full max-h-96 rounded-md" preload="metadata">{asset.title || "生成的视频"}</video>;
        }
        if (asset.kind === "audio") {
          return <audio key={`${message.id}-media-${index}`} src={src} controls className="w-full" preload="metadata">{asset.title || "生成的音频"}</audio>;
        }
        return <a key={`${message.id}-media-${index}`} href={src} className="text-blue-400 hover:text-blue-300 underline text-sm" target="_blank" rel="noopener noreferrer">{asset.title || asset.url}</a>;
      })}
    </div>
  );
}
