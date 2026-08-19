param(
    [Parameter(Mandatory = $true)]
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class ScreenshotDpi {
    [DllImport("user32.dll")]
    public static extern bool SetProcessDPIAware();
}
"@
[ScreenshotDpi]::SetProcessDPIAware() | Out-Null

$bounds = [System.Windows.Forms.SystemInformation]::VirtualScreen
$desktop = [System.Drawing.Bitmap]::new(
    $bounds.Width,
    $bounds.Height,
    [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
)
$graphics = [System.Drawing.Graphics]::FromImage($desktop)
$graphics.CopyFromScreen(
    $bounds.Left,
    $bounds.Top,
    0,
    0,
    $bounds.Size,
    [System.Drawing.CopyPixelOperation]::SourceCopy
)
$graphics.Dispose()

$form = New-Object System.Windows.Forms.Form
$form.FormBorderStyle = [System.Windows.Forms.FormBorderStyle]::None
$form.StartPosition = [System.Windows.Forms.FormStartPosition]::Manual
$form.Bounds = $bounds
$form.TopMost = $true
$form.ShowInTaskbar = $false
$form.KeyPreview = $true
$form.Cursor = [System.Windows.Forms.Cursors]::Cross
$form.BackgroundImage = $desktop
$form.BackgroundImageLayout = [System.Windows.Forms.ImageLayout]::None

$state = @{
    Selecting = $false
    Captured = $false
    Start = [System.Drawing.Point]::Empty
    Current = [System.Drawing.Point]::Empty
}

function Get-SelectionRectangle {
    $left = [Math]::Min($state.Start.X, $state.Current.X)
    $top = [Math]::Min($state.Start.Y, $state.Current.Y)
    $right = [Math]::Max($state.Start.X, $state.Current.X)
    $bottom = [Math]::Max($state.Start.Y, $state.Current.Y)
    return [System.Drawing.Rectangle]::FromLTRB($left, $top, $right, $bottom)
}

$form.Add_Shown({
    $form.Activate()
    $form.Focus()
})
$form.Add_KeyDown({
    param($sender, $eventArgs)
    if ($eventArgs.KeyCode -eq [System.Windows.Forms.Keys]::Escape) {
        $form.Close()
    }
})
$form.Add_MouseDown({
    param($sender, $eventArgs)
    if ($eventArgs.Button -eq [System.Windows.Forms.MouseButtons]::Left) {
        $state.Selecting = $true
        $state.Start = $eventArgs.Location
        $state.Current = $eventArgs.Location
        $form.Invalidate()
    }
})
$form.Add_MouseMove({
    param($sender, $eventArgs)
    if ($state.Selecting) {
        $state.Current = $eventArgs.Location
        $form.Invalidate()
    }
})
$form.Add_MouseUp({
    param($sender, $eventArgs)
    if (-not $state.Selecting -or $eventArgs.Button -ne [System.Windows.Forms.MouseButtons]::Left) {
        return
    }
    $state.Selecting = $false
    $state.Current = $eventArgs.Location
    $selection = Get-SelectionRectangle
    if ($selection.Width -lt 2 -or $selection.Height -lt 2) {
        $form.Invalidate()
        return
    }

    $cropped = [System.Drawing.Bitmap]::new(
        $selection.Width,
        $selection.Height,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
    )
    $cropGraphics = [System.Drawing.Graphics]::FromImage($cropped)
    $destination = [System.Drawing.Rectangle]::new(0, 0, $selection.Width, $selection.Height)
    $cropGraphics.DrawImage(
        $desktop,
        $destination,
        $selection,
        [System.Drawing.GraphicsUnit]::Pixel
    )
    $cropGraphics.Dispose()
    $cropped.Save($OutputPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $cropped.Dispose()
    $state.Captured = $true
    $form.Close()
})
$form.Add_Paint({
    param($sender, $eventArgs)
    $shade = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(112, 0, 0, 0))
    $eventArgs.Graphics.FillRectangle($shade, $form.ClientRectangle)
    $shade.Dispose()
    if ($state.Selecting) {
        $selection = Get-SelectionRectangle
        if ($selection.Width -gt 0 -and $selection.Height -gt 0) {
            $eventArgs.Graphics.DrawImage(
                $desktop,
                $selection,
                $selection,
                [System.Drawing.GraphicsUnit]::Pixel
            )
            $pen = New-Object System.Drawing.Pen([System.Drawing.Color]::White, 1)
            $eventArgs.Graphics.DrawRectangle($pen, $selection)
            $pen.Dispose()
        }
    }
})

[System.Windows.Forms.Application]::Run($form)
$form.Dispose()
$desktop.Dispose()

if ($state.Captured) {
    exit 0
}
exit 2
