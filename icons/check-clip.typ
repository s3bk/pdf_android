#set page(width: 512pt, height: 512pt, margin: 0pt)
// emulates the launcher: rust background layer, the fg layer on top, then a
// circular mask of the size measured on the emulator (r = 238 of 512)
#place(top+left, rect(width: 512pt, height: 512pt, fill: rgb("#e43717")))
#place(top+left, image("/home/hematite/work/pdf-android/android/res/drawable/ic_launcher_foreground.xml", width: 512pt, height: 512pt))
