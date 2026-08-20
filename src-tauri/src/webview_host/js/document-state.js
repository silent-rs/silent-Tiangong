(function() {
    try {
        return {
            document_id: String(window.__tiangong_document_id || ''),
            ready_state: String(document.readyState || ''),
            url: String(window.location.href || ''),
            title: String(document.title || ''),
            has_content: !!(
                document.body &&
                String(document.body.innerText || '').trim().length > 0
            ),
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
            has_content: false,
            internal_error: false
        };
    }
})()
