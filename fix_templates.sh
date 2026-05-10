#!/bin/bash
cd /Users/macos/Documents/Polaris/web/templates

# Fix base.html
sed -i '' 's/{{template "main" .}}/{{embed}}/' base.html

# Fix child templates
for file in home.html post.html page.html archives.html categories.html category.html tags.html tag.html search.html about.html 404.html; do
    echo "Processing $file"
    # Remove {{template "base" .}} and {{define "main"}}
    sed -i '' -e '/{{template "base" \.}}/d' -e '/{{define "main"}}/d' "$file"
    
    # Remove the very last {{end}}
    # We can do this efficiently by reversing, replacing the first occurrence, and reversing back
    tail -r "$file" | sed '1,1s/{{end}}//' | tail -r > "${file}.tmp"
    mv "${file}.tmp" "$file"
done
