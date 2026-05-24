package theme

import (
	"strings"
)

type BrowserInfo struct {
	Browser string `json:"browser"`
	OS      string `json:"os"`
}

func parseBrowserInfo(ua string) BrowserInfo {
	info := BrowserInfo{
		Browser: ParseBrowser(ua),
		OS:      ParseOS(ua),
	}
	return info
}

func ParseBrowser(ua string) string {
	switch {
	case strings.Contains(ua, "Firefox/"):
		return "Firefox"
	case strings.Contains(ua, "Edg/"):
		return "Edge"
	case strings.Contains(ua, "OPR/") || strings.Contains(ua, "Opera"):
		return "Opera"
	case strings.Contains(ua, "Vivaldi/"):
		return "Vivaldi"
	case strings.Contains(ua, "Brave/"):
		return "Brave"
	case strings.Contains(ua, "SamsungBrowser/"):
		return "Samsung Browser"
	case strings.Contains(ua, "Chrome/") && !strings.Contains(ua, "Edg/"):
		return "Chrome"
	case strings.Contains(ua, "Safari/") && !strings.Contains(ua, "Chrome/"):
		return "Safari"
	case strings.Contains(ua, "MSIE") || strings.Contains(ua, "Trident/"):
		return "IE"
	default:
		return "Unknown"
	}
}

func ParseOS(ua string) string {
	switch {
	case strings.Contains(ua, "Windows NT 10"):
		return "Windows 10/11"
	case strings.Contains(ua, "Windows NT 6.3"):
		return "Windows 8.1"
	case strings.Contains(ua, "Windows NT 6.2"):
		return "Windows 8"
	case strings.Contains(ua, "Windows NT 6.1"):
		return "Windows 7"
	case strings.Contains(ua, "Windows"):
		return "Windows"
	case strings.Contains(ua, "Mac OS X"):
		return "macOS"
	case strings.Contains(ua, "iPhone") || strings.Contains(ua, "iPad"):
		return "iOS"
	case strings.Contains(ua, "Android"):
		return "Android"
	case strings.Contains(ua, "Linux"):
		return "Linux"
	case strings.Contains(ua, "CrOS"):
		return "ChromeOS"
	default:
		return "Unknown"
	}
}
