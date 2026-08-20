(function() {
    try {
        var bridge = window.__tiangong_bridge;
        var page = bridge && bridge.getFullText
            ? bridge.getFullText(12000)
            : {
                title: document.title || '',
                url: window.location.href || '',
                text: ''
            };
        return {
            document_id: String(window.__tiangong_document_id || ''),
            ready_state: String(document.readyState || ''),
            url: String(page.url || window.location.href || ''),
            title: String(page.title || document.title || ''),
            text: String(page.text || ''),
            internal_error: !!(
                document.documentElement &&
                document.documentElement.getAttribute('data-tiangong-navigation-error') === 'true'
            )
        };
    } catch (error) {
        return {
            document_id: '',
            ready_state: '',
            url: '',
            title: '',
            text: '',
            internal_error: false
        };
    }
})()
