; Windows caches Start-menu artwork by icon path. Give each release a new icon
; path, then recreate existing shortcuts so upgrades show the current artwork.
!macro ORPHEUS_REFRESH_SHORTCUT SHORTCUT_PATH
  ${If} ${FileExists} "${SHORTCUT_PATH}"
    Delete "${SHORTCUT_PATH}"
    CreateShortcut "${SHORTCUT_PATH}" "$INSTDIR\${MAINBINARYNAME}.exe" "" "$INSTDIR\orpheus-pet-icon-${VERSION}.ico" 0
    !insertmacro SetLnkAppUserModelId "${SHORTCUT_PATH}"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  Delete "$INSTDIR\orpheus-pet-icon-*.ico"
  CopyFiles /SILENT "$INSTDIR\orpheus-pet-icon.ico" "$INSTDIR\orpheus-pet-icon-${VERSION}.ico"

  !if "${STARTMENUFOLDER}" != ""
    !insertmacro ORPHEUS_REFRESH_SHORTCUT "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
  !else
    !insertmacro ORPHEUS_REFRESH_SHORTCUT "$SMPROGRAMS\${PRODUCTNAME}.lnk"
  !endif
  !insertmacro ORPHEUS_REFRESH_SHORTCUT "$DESKTOP\${PRODUCTNAME}.lnk"

  WriteRegStr SHCTX "${UNINSTKEY}" "DisplayIcon" "$\"$INSTDIR\orpheus-pet-icon-${VERSION}.ico$\""
  System::Call 'shell32::SHChangeNotify(i ${SHCNE_ASSOCCHANGED}, i ${SHCNF_FLUSH}, p 0, p 0)'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  Delete "$INSTDIR\orpheus-pet-icon-*.ico"
  RMDir "$INSTDIR"
!macroend
