(function() {
    try {
        return {
            document_id: String(window.__tiangong_document_id || ''),
            ready_state: String(document.readyState || ''),
            url: String(window.location.href || ''),
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
            internal_error: false
        };
    }
})()
