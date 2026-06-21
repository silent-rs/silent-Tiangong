import { resolveAssetUrl } from "./utils";
import type { MessageItem } from "./types";

export function ContentMedia({ message }: { message: MessageItem }) {
  const content = Array.isArray(message.content) ? message.content : [];
  const mediaBlocks = content.filter((b) => b.type === "media");
  const legacyMedia = message.media || [];
  const allMedia = [
    ...mediaBlocks.map((b) => ({ kind: b.kind!, url: b.url!, title: b.title, mime_type: b.mime_type })),
    ...legacyMedia,
  ];
  if (allMedia.length === 0) return null;
  return (
    <div className="space-y-2 my-2">
      {allMedia.map((asset, index) => {
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
